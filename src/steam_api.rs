use anyhow::{Context, Result};
use reqwest::blocking::Client;
use scraper::{Html, Selector};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

const DETAILS_URL: &str =
    "https://api.steampowered.com/ISteamRemoteStorage/GetPublishedFileDetails/v1/";

#[derive(Clone, Debug)]
pub struct WorkshopDetails {
    pub workshop_id: u64,
    pub title: String,
    pub description: String,
    pub preview_url: Option<String>,
    pub preview_image: Option<Vec<u8>>,
    pub time_created: Option<u64>,
    pub time_updated: Option<u64>,
    pub file_size: Option<u64>,
    pub subscriptions: Option<u64>,
    pub favorited: Option<u64>,
    pub views: Option<u64>,
    pub tags: Vec<String>,
    pub creators: Vec<WorkshopCreator>,
    pub required_items: Vec<WorkshopRequiredItem>,
}

#[derive(Clone, Debug)]
pub struct WorkshopCreator {
    pub name: String,
    pub profile_url: String,
}

#[derive(Clone, Debug)]
pub struct WorkshopRequiredItem {
    pub workshop_id: Option<u64>,
    pub title: String,
    pub url: String,
}

#[derive(Clone, Debug)]
pub struct WorkshopSummary {
    pub title: String,
    pub time_updated: Option<u64>,
}

#[derive(Deserialize)]
struct SteamProfile {
    #[serde(rename = "steamID")]
    steam_id: Option<String>,
}

#[derive(Default)]
struct WorkshopPageInfo {
    creators: Vec<WorkshopCreator>,
    required_items: Vec<WorkshopRequiredItem>,
}

pub fn fetch_workshop_details(workshop_id: u64) -> Result<WorkshopDetails> {
    let client = Client::builder()
        .user_agent("isaac_mod_manager")
        .timeout(Duration::from_secs(20))
        .build()?;

    let response: Value = client
        .post(DETAILS_URL)
        .form(&[
            ("itemcount", "1".to_string()),
            ("publishedfileids[0]", workshop_id.to_string()),
        ])
        .send()
        .context("Failed to request Steam Workshop details")?
        .error_for_status()
        .context("Steam Workshop details request failed")?
        .json()
        .context("Failed to decode Steam Workshop details")?;

    let item = response
        .get("response")
        .and_then(|response| response.get("publishedfiledetails"))
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .context("Steam Workshop details response was empty")?;

    let result = value_u64(item, "result").unwrap_or(0);
    if result != 1 {
        return Err(anyhow::anyhow!(
            "Steam Workshop details returned result code {}",
            result
        ));
    }

    let preview_url = value_string(item, "preview_url");
    let preview_image = match preview_url.as_deref() {
        Some(url) => client
            .get(url)
            .send()
            .and_then(|response| response.error_for_status())
            .and_then(|response| response.bytes())
            .map(|bytes| bytes.to_vec())
            .ok(),
        None => None,
    };

    let tags = item
        .get("tags")
        .and_then(Value::as_array)
        .map(|tags| {
            tags.iter()
                .filter_map(|tag| value_string(tag, "tag"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let creator_steam_id = value_string(item, "creator");
    let mut page_info = fetch_workshop_page_info(&client, workshop_id).unwrap_or_default();
    if page_info.creators.is_empty() {
        if let Some(steam_id) = creator_steam_id.as_deref() {
            let name = fetch_steam_profile_name(&client, steam_id)
                .unwrap_or_else(|_| steam_id.to_string());
            page_info.creators.push(WorkshopCreator {
                name,
                profile_url: steam_profile_url(steam_id),
            });
        }
    }

    Ok(WorkshopDetails {
        workshop_id,
        title: value_string(item, "title").unwrap_or_else(|| format!("Workshop {}", workshop_id)),
        description: value_string(item, "description")
            .map(|description| clean_description(&description))
            .unwrap_or_else(|| "No description provided.".to_string()),
        preview_url,
        preview_image,
        time_created: value_u64(item, "time_created"),
        time_updated: value_u64(item, "time_updated"),
        file_size: value_u64(item, "file_size"),
        subscriptions: value_u64(item, "subscriptions"),
        favorited: value_u64(item, "favorited"),
        views: value_u64(item, "views"),
        tags,
        creators: page_info.creators,
        required_items: page_info.required_items,
    })
}

pub fn fetch_workshop_summaries(workshop_ids: &[u64]) -> Result<HashMap<u64, WorkshopSummary>> {
    let mut ids = workshop_ids
        .iter()
        .copied()
        .filter(|id| *id > 0)
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();

    let client = Client::builder()
        .user_agent("isaac_mod_manager")
        .timeout(Duration::from_secs(8))
        .build()?;

    let mut output = HashMap::new();
    for chunk in ids.chunks(100) {
        let mut form = vec![("itemcount".to_string(), chunk.len().to_string())];
        for (index, workshop_id) in chunk.iter().enumerate() {
            form.push((
                format!("publishedfileids[{}]", index),
                workshop_id.to_string(),
            ));
        }

        let response: Value = client
            .post(DETAILS_URL)
            .form(&form)
            .send()
            .context("Failed to request Steam Workshop summaries")?
            .error_for_status()
            .context("Steam Workshop summaries request failed")?
            .json()
            .context("Failed to decode Steam Workshop summaries")?;

        let Some(items) = response
            .get("response")
            .and_then(|response| response.get("publishedfiledetails"))
            .and_then(Value::as_array)
        else {
            continue;
        };

        for item in items {
            if value_u64(item, "result") != Some(1) {
                continue;
            }
            let Some(workshop_id) = value_u64(item, "publishedfileid") else {
                continue;
            };

            output.insert(
                workshop_id,
                WorkshopSummary {
                    title: value_string(item, "title")
                        .unwrap_or_else(|| format!("Workshop {}", workshop_id)),
                    time_updated: value_u64(item, "time_updated"),
                },
            );
        }
    }

    Ok(output)
}

fn fetch_workshop_page_info(client: &Client, workshop_id: u64) -> Result<WorkshopPageInfo> {
    let html = client
        .get(format!(
            "https://steamcommunity.com/sharedfiles/filedetails/?id={}&l=english",
            workshop_id
        ))
        .send()
        .context("Failed to request Steam Workshop page")?
        .error_for_status()
        .context("Steam Workshop page request failed")?
        .text()
        .context("Failed to read Steam Workshop page")?;

    let document = Html::parse_document(&html);
    Ok(WorkshopPageInfo {
        creators: parse_workshop_creators(&document),
        required_items: parse_required_items(&document),
    })
}

fn parse_workshop_creators(document: &Html) -> Vec<WorkshopCreator> {
    let block_selector = Selector::parse(".creatorsBlock .friendBlock").expect("valid selector");
    let link_selector = Selector::parse("a.friendBlockLinkOverlay").expect("valid selector");
    let content_selector = Selector::parse(".friendBlockContent").expect("valid selector");

    let mut creators = Vec::new();
    for block in document.select(&block_selector) {
        let Some(profile_url) = block
            .select(&link_selector)
            .next()
            .and_then(|link| link.value().attr("href"))
            .and_then(normalize_steam_profile_url)
        else {
            continue;
        };

        let name = block
            .select(&content_selector)
            .next()
            .and_then(|content| {
                content
                    .text()
                    .map(str::trim)
                    .find(|text| !text.is_empty() && !is_presence_text(text))
                    .map(ToOwned::to_owned)
            })
            .unwrap_or_else(|| profile_url.clone());

        if !creators
            .iter()
            .any(|creator: &WorkshopCreator| creator.profile_url.eq_ignore_ascii_case(&profile_url))
        {
            creators.push(WorkshopCreator { name, profile_url });
        }
    }

    creators
}

fn parse_required_items(document: &Html) -> Vec<WorkshopRequiredItem> {
    let item_selector =
        Selector::parse("#RequiredItems a[href] .requiredItem").expect("valid selector");
    let mut required_items = Vec::new();

    for item in document.select(&item_selector) {
        let Some(parent) = item.parent().and_then(scraper::ElementRef::wrap) else {
            continue;
        };
        let Some(url) = parent.value().attr("href").and_then(normalize_workshop_url) else {
            continue;
        };

        let title = item
            .text()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string();
        if title.is_empty() {
            continue;
        }

        if required_items
            .iter()
            .any(|required_item: &WorkshopRequiredItem| {
                required_item.url.eq_ignore_ascii_case(&url)
            })
        {
            continue;
        }

        required_items.push(WorkshopRequiredItem {
            workshop_id: workshop_id_from_url(&url),
            title,
            url,
        });
    }

    required_items
}

fn normalize_workshop_url(href: &str) -> Option<String> {
    let trimmed = href.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.starts_with("https://steamcommunity.com/sharedfiles/filedetails/")
        || trimmed.starts_with("https://steamcommunity.com/workshop/filedetails/")
    {
        return Some(trimmed.to_string());
    }

    if trimmed.starts_with("/sharedfiles/filedetails/")
        || trimmed.starts_with("/workshop/filedetails/")
    {
        return Some(format!("https://steamcommunity.com{}", trimmed));
    }

    None
}

fn workshop_id_from_url(url: &str) -> Option<u64> {
    let marker = "id=";
    let index = url.find(marker)?;
    url[index + marker.len()..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>()
        .parse::<u64>()
        .ok()
        .filter(|id| *id > 0)
}

fn is_presence_text(text: &str) -> bool {
    matches!(
        text.to_ascii_lowercase().as_str(),
        "offline" | "online" | "in-game" | "in game" | "away" | "snooze"
    )
}

fn normalize_steam_profile_url(href: &str) -> Option<String> {
    let trimmed = href.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.starts_with("https://steamcommunity.com/id/")
        || trimmed.starts_with("https://steamcommunity.com/profiles/")
    {
        return Some(trimmed.to_string());
    }

    if trimmed.starts_with("/id/") || trimmed.starts_with("/profiles/") {
        return Some(format!("https://steamcommunity.com{}", trimmed));
    }

    None
}

fn steam_profile_url(steam_id: &str) -> String {
    format!("https://steamcommunity.com/profiles/{}", steam_id)
}

fn fetch_steam_profile_name(client: &Client, steam_id: &str) -> Result<String> {
    let body = client
        .get(format!(
            "https://steamcommunity.com/profiles/{}/?xml=1",
            steam_id
        ))
        .send()
        .context("Failed to request Steam profile")?
        .error_for_status()
        .context("Steam profile request failed")?
        .text()
        .context("Failed to read Steam profile")?;
    let profile: SteamProfile =
        quick_xml::de::from_str(&body).context("Failed to decode Steam profile")?;

    profile
        .steam_id
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .context("Steam profile did not include a display name")
}

fn value_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(|value| match value {
        Value::String(text) => {
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_string())
        }
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    })
}

fn value_u64(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(|value| match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    })
}

fn clean_description(description: &str) -> String {
    let normalized = description
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">");

    let mut cleaned = String::with_capacity(normalized.len());
    let mut in_tag = false;
    for ch in normalized.chars() {
        match ch {
            '[' => in_tag = true,
            ']' if in_tag => {
                in_tag = false;
                cleaned.push('\n');
            }
            _ if !in_tag => cleaned.push(ch),
            _ => {}
        }
    }

    let mut output = String::new();
    let mut blank_lines = 0;
    for line in cleaned.lines() {
        let line = line.trim();
        if line.is_empty() {
            blank_lines += 1;
            if blank_lines <= 1 {
                output.push('\n');
            }
        } else {
            blank_lines = 0;
            output.push_str(line);
            output.push('\n');
        }
    }

    let output = output.trim();
    if output.is_empty() {
        "No description provided.".to_string()
    } else {
        output.to_string()
    }
}
