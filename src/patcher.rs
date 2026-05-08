use crate::steam_workshop::SteamWorkshopClient;
use anyhow::Result;
use encoding_rs::EUC_KR;
use serde::Deserialize;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Deserialize, Debug)]
struct LocalMetadata {
    version: Option<String>,
}

pub struct Patcher {
    mod_path: PathBuf,
    allow_downgrade: bool,
}

impl Patcher {
    pub fn new(_client: SteamWorkshopClient, mod_path: PathBuf) -> Self {
        Self {
            mod_path,
            allow_downgrade: false,
        }
    }

    pub fn allow_downgrade(mut self, allow_downgrade: bool) -> Self {
        self.allow_downgrade = allow_downgrade;
        self
    }

    pub fn sync_from_source_dir<F>(&self, source_dir: &Path, logger: Option<F>) -> Result<()>
    where
        F: Fn(String),
    {
        self.sync_from_source_dir_with_logger(
            source_dir,
            logger.as_ref().map(|f| f as &dyn Fn(String)),
        )
    }

    fn sync_from_source_dir_with_logger(
        &self,
        source_dir: &Path,
        logger: Option<&dyn Fn(String)>,
    ) -> Result<()> {
        log(
            logger,
            "Step 1/3: Checking installed version...".to_string(),
        );
        let local_version = self.read_local_version(logger);
        self.sync_source_with_local_version(source_dir, local_version, logger)
    }

    fn read_local_version(&self, logger: Option<&dyn Fn(String)>) -> Option<String> {
        let local_metadata = match read_local_metadata(&self.mod_path) {
            Ok(metadata) => metadata,
            Err(e) => {
                log(
                    logger,
                    format!("Local metadata unreadable; forcing update: {}", e),
                );
                None
            }
        };

        local_metadata
            .as_ref()
            .and_then(|metadata| normalize_version(metadata.version.as_deref()))
    }

    fn sync_source_with_local_version(
        &self,
        workshop_path: &Path,
        local_version: Option<String>,
        logger: Option<&dyn Fn(String)>,
    ) -> Result<()> {
        log(
            logger,
            "Step 3/4: Reading downloaded workshop metadata...".to_string(),
        );
        let workshop_metadata = read_local_metadata(workshop_path)?;
        let workshop_version = workshop_metadata
            .as_ref()
            .and_then(|metadata| normalize_version(metadata.version.as_deref()));

        match (local_version.as_deref(), workshop_version.as_deref()) {
            (Some(local), Some(remote)) if local == remote => {
                log(logger, format!("Already up to date (version {}).", local));
                Ok(())
            }
            (Some(local), Some(remote))
                if !self.allow_downgrade
                    && compare_version_strings(local, remote) == Some(Ordering::Greater) =>
            {
                Err(anyhow::anyhow!(
                    "Local version {} is newer than Steam version {}. Confirm before matching Steam version.",
                    local,
                    remote
                ))
            }
            (local, Some(remote)) => {
                log(
                    logger,
                    format!(
                        "Update required: {} -> {}",
                        local.unwrap_or("missing"),
                        remote
                    ),
                );
                self.sync_from_dir(workshop_path, logger)
            }
            (_, None) => {
                log(
                    logger,
                    "Workshop metadata has no version; syncing downloaded content.".to_string(),
                );
                self.sync_from_dir(workshop_path, logger)
            }
        }
    }

    fn sync_from_dir(&self, source_dir: &Path, logger: Option<&dyn Fn(String)>) -> Result<()> {
        log(
            logger,
            "Step 4/4: Applying downloaded files to selected mod folder...".to_string(),
        );

        let mut processed_files = HashSet::new();
        for entry in walkdir::WalkDir::new(source_dir)
            .into_iter()
            .filter_map(|entry| entry.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }

            let source_path = entry.path();
            let relative_path = source_path.strip_prefix(source_dir)?;
            if should_skip(relative_path) {
                continue;
            }

            let target_path = self.mod_path.join(relative_path);
            processed_files.insert(target_path.clone());

            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent)?;
            }

            let content = fs::read(source_path)?;
            let is_different = fs::read(&target_path)
                .map(|local_content| local_content != content)
                .unwrap_or(true);

            if is_different {
                if target_path.exists() {
                    log(logger, format!("Updated: {}", relative_path.display()));
                } else {
                    log(logger, format!("New: {}", relative_path.display()));
                }
                fs::write(&target_path, content)?;
            }
        }

        log(
            logger,
            "Cleaning up files removed from workshop content...".to_string(),
        );
        for entry in walkdir::WalkDir::new(&self.mod_path)
            .into_iter()
            .filter_map(|entry| entry.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.path().to_path_buf();
            if processed_files.contains(&path) {
                continue;
            }

            let Ok(relative_path) = path.strip_prefix(&self.mod_path) else {
                continue;
            };
            if should_skip(relative_path) {
                continue;
            }

            log(logger, format!("Deleted: {}", relative_path.display()));
            let _ = fs::remove_file(path);
        }

        log(logger, "Update complete!".to_string());
        Ok(())
    }
}

fn should_skip(relative_path: &Path) -> bool {
    let file_name = relative_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();

    file_name == ".DS_Store" || file_name == "Thumbs.db"
}

fn log(logger: Option<&dyn Fn(String)>, msg: String) {
    if let Some(f) = logger {
        f(msg.clone());
    }
    println!("{}", msg);
}

fn read_local_metadata(root: &Path) -> Result<Option<LocalMetadata>> {
    let metadata_path = root.join("metadata.xml");
    if !metadata_path.exists() {
        return Ok(None);
    }

    let content = read_text_file(&metadata_path)?;
    let metadata = quick_xml::de::from_str(&content)?;
    Ok(Some(metadata))
}

fn read_text_file(path: &Path) -> std::io::Result<String> {
    let bytes = fs::read(path)?;
    Ok(decode_text_bytes(&bytes))
}

fn decode_text_bytes(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => {
            let (decoded, _, _) = EUC_KR.decode(bytes);
            decoded.into_owned()
        }
    }
}

fn normalize_version(version: Option<&str>) -> Option<String> {
    version
        .map(str::trim)
        .filter(|version| !version.is_empty())
        .map(ToOwned::to_owned)
}

fn compare_version_strings(left: &str, right: &str) -> Option<Ordering> {
    if left.trim() == right.trim() {
        return Some(Ordering::Equal);
    }

    let left_parts = numeric_version_parts(left);
    let right_parts = numeric_version_parts(right);
    if left_parts.is_empty() || right_parts.is_empty() {
        return None;
    }

    let len = left_parts.len().max(right_parts.len());
    for index in 0..len {
        let left = *left_parts.get(index).unwrap_or(&0);
        let right = *right_parts.get(index).unwrap_or(&0);
        match left.cmp(&right) {
            Ordering::Equal => {}
            ordering => return Some(ordering),
        }
    }

    Some(Ordering::Equal)
}

fn numeric_version_parts(version: &str) -> Vec<u64> {
    let mut parts = Vec::new();
    let mut current = String::new();

    for ch in version.chars() {
        if ch.is_ascii_digit() {
            current.push(ch);
        } else if !current.is_empty() {
            if let Ok(value) = current.parse::<u64>() {
                parts.push(value);
            }
            current.clear();
        }
    }

    if !current.is_empty() {
        if let Ok(value) = current.parse::<u64>() {
            parts.push(value);
        }
    }

    parts
}
