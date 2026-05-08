use crate::fs_utils::{find_isaac_game_path, find_steam_library_roots};
use crate::patcher::Patcher;
use crate::steam_api::{fetch_workshop_details, fetch_workshop_summaries, WorkshopDetails};
use crate::steam_workshop::{
    find_cached_workshop_item, find_steamcmd, prepare_steamcmd, SteamWorkshopClient,
    CONCH_BLESSING_WORKSHOP_ID, ISAAC_APP_ID,
};
use chrono::{DateTime, Local};
use eframe::egui;
use encoding_rs::EUC_KR;
use serde::Deserialize;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, UNIX_EPOCH};

const SUPPORTED_MOD_DIRECTORY: &str = "conch_blessing";
const APP_TITLE: &str = "Isaac Mod Manager";
const MIN_VISIBLE_WIDTH: f32 = 1040.0;
const MIN_VISIBLE_HEIGHT: f32 = 780.0;
const DESCRIPTION_MIN_HEIGHT: f32 = 280.0;
const ACTIONS_PANEL_HEIGHT: f32 = 58.0;
const LOG_PANEL_MIN_HEIGHT: f32 = 90.0;
const LOG_PANEL_DEFAULT_HEIGHT: f32 = 180.0;
const LOG_PANEL_MAX_HEIGHT: f32 = 230.0;
const SINGLE_STEAM_CLIENT_WAIT_SECS: u64 = 20;
const BULK_STEAM_CLIENT_WAIT_SECS: u64 = 20;
const SETTINGS_REGISTRY_KEY: &str = "Software\\Ba-koD\\isaac_mod_manager";
const LEGACY_SETTINGS_REGISTRY_KEY: &str = "Software\\Ba-koD\\cb_patcher";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LanguageMode {
    System,
    English,
    Korean,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UiLanguage {
    English,
    Korean,
}

#[derive(Default)]
enum AppState {
    #[default]
    Idle,
    Checking,
    Syncing,
    Done,
    Error,
}

#[derive(Clone, Debug)]
struct InstalledMod {
    path: PathBuf,
    folder_name: String,
    name: Option<String>,
    version: Option<String>,
    description: Option<String>,
    author: Option<String>,
    workshop_id: Option<u64>,
    steam_version: Option<String>,
    steam_title: Option<String>,
    steam_updated_at: Option<u64>,
    update_status: ModUpdateStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ModUpdateStatus {
    Latest,
    Outdated,
    LocalNewer,
    OnlineAvailable,
    MissingSteamCache,
    Unknown,
    LocalOnly,
}

#[derive(Clone, Debug)]
struct PendingConfirmation {
    indices: Vec<usize>,
    force_update: bool,
}

#[derive(Clone, Debug)]
struct PendingSubscribeNotice {
    workshop_id: u64,
}

#[derive(Clone, Debug)]
struct UpdateProgress {
    total: usize,
    completed: usize,
    current_mod: Option<String>,
    current_detail: Option<String>,
    current_percent: f32,
}

impl Default for UpdateProgress {
    fn default() -> Self {
        Self {
            total: 0,
            completed: 0,
            current_mod: None,
            current_detail: None,
            current_percent: 0.0,
        }
    }
}

#[derive(Clone)]
struct UpdateTarget {
    path: PathBuf,
    workshop_id: u64,
    display_name: String,
}

#[derive(Clone)]
struct UpdateGroup {
    workshop_id: u64,
    targets: Vec<UpdateTarget>,
}

#[derive(Clone, Debug)]
enum WorkshopDetailsState {
    Loading,
    Ready(WorkshopDetails),
    Error(String),
}

#[derive(Clone, Debug)]
enum DependencyCheckState {
    NotRun,
    Checking,
    Ready(DependencyReport),
    Error(String),
}

#[derive(Clone, Debug)]
struct DependencyReport {
    steam_path: Option<PathBuf>,
    isaac_path: Option<PathBuf>,
    steam_library_roots: Vec<PathBuf>,
    workshop_cache_roots: usize,
    steamcmd_path: Option<PathBuf>,
    steamcmd_error: Option<String>,
    steam_web_api_error: Option<String>,
}

#[derive(Deserialize, Default)]
struct LocalMetadata {
    name: Option<String>,
    directory: Option<String>,
    id: Option<String>,
    version: Option<String>,
    description: Option<String>,
    author: Option<String>,
}

impl InstalledMod {
    fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.folder_name)
    }

    fn version_label(&self) -> &str {
        self.version.as_deref().unwrap_or("unknown")
    }

    fn row_label(&self, language: UiLanguage) -> String {
        format!(
            "[{}] {}",
            self.update_status.label(language),
            self.display_name()
        )
    }
}

impl ModUpdateStatus {
    fn label(&self, language: UiLanguage) -> &'static str {
        match (language, self) {
            (UiLanguage::Korean, Self::Latest) => "최신",
            (UiLanguage::Korean, Self::Outdated) => "업데이트 필요",
            (UiLanguage::Korean, Self::LocalNewer) => "로컬 버전 높음",
            (UiLanguage::Korean, Self::OnlineAvailable) => "온라인 확인됨",
            (UiLanguage::Korean, Self::MissingSteamCache) => "Steam 미다운로드",
            (UiLanguage::Korean, Self::Unknown) => "확인 불가",
            (UiLanguage::Korean, Self::LocalOnly) => "로컬 전용",
            (_, Self::Latest) => "Latest",
            (_, Self::Outdated) => "Outdated",
            (_, Self::LocalNewer) => "Local newer",
            (_, Self::OnlineAvailable) => "Online available",
            (_, Self::MissingSteamCache) => "Steam not downloaded",
            (_, Self::Unknown) => "Unknown",
            (_, Self::LocalOnly) => "Local only",
        }
    }

    fn color(&self) -> egui::Color32 {
        match self {
            Self::Latest => egui::Color32::from_rgb(80, 170, 100),
            Self::Outdated => egui::Color32::from_rgb(230, 140, 45),
            Self::LocalNewer => egui::Color32::from_rgb(120, 130, 235),
            Self::OnlineAvailable => egui::Color32::from_rgb(90, 150, 220),
            Self::MissingSteamCache => egui::Color32::from_rgb(170, 150, 80),
            Self::Unknown => egui::Color32::from_rgb(150, 150, 150),
            Self::LocalOnly => egui::Color32::from_rgb(130, 130, 130),
        }
    }

    fn is_update_candidate(&self) -> bool {
        matches!(
            self,
            Self::Outdated
                | Self::LocalNewer
                | Self::OnlineAvailable
                | Self::MissingSteamCache
                | Self::Unknown
        )
    }
}

impl LanguageMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::English => "english",
            Self::Korean => "korean",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "system" => Some(Self::System),
            "english" => Some(Self::English),
            "korean" => Some(Self::Korean),
            _ => None,
        }
    }

    fn label(self, language: UiLanguage) -> &'static str {
        match (language, self) {
            (UiLanguage::Korean, Self::System) => "시스템",
            (UiLanguage::Korean, Self::English) => "영어",
            (UiLanguage::Korean, Self::Korean) => "한국어",
            (_, Self::System) => "System",
            (_, Self::English) => "English",
            (_, Self::Korean) => "Korean",
        }
    }
}

pub struct PatcherApp {
    game_path: Option<PathBuf>,
    target_mod_path: Option<PathBuf>,
    available_mods: Vec<InstalledMod>,
    selected_mod_index: Option<usize>,
    state: AppState,
    status_message: String,
    progress_log: Arc<Mutex<Vec<String>>>,
    update_progress: Arc<Mutex<UpdateProgress>>,
    app_id: u32,
    auto_update_enabled: bool,
    auto_update_exclusions: HashSet<u64>,
    checked_update_paths: HashSet<PathBuf>,
    update_selection_touched: bool,
    force_update_enabled: bool,
    show_log: bool,
    language_mode: LanguageMode,
    pending_confirmation: Option<PendingConfirmation>,
    pending_subscribe_notice: Option<PendingSubscribeNotice>,
    show_force_update_notice: bool,
    shown_subscribe_notices: HashSet<u64>,
    search_query: String,
    details_cache: Arc<Mutex<HashMap<u64, WorkshopDetailsState>>>,
    preview_textures: HashMap<u64, egui::TextureHandle>,
    preview_failures: HashSet<u64>,
    dependency_check: Arc<Mutex<DependencyCheckState>>,
    show_dependency_check: bool,
}

impl Default for PatcherApp {
    fn default() -> Self {
        let language_mode = load_language_mode().unwrap_or(LanguageMode::System);
        let language = match language_mode {
            LanguageMode::English => UiLanguage::English,
            LanguageMode::Korean => UiLanguage::Korean,
            LanguageMode::System => system_language(),
        };
        let mut app = Self {
            game_path: None,
            target_mod_path: None,
            available_mods: Vec::new(),
            selected_mod_index: None,
            state: AppState::Idle,
            status_message: tr(language, "ready").to_string(),
            progress_log: Arc::new(Mutex::new(Vec::new())),
            update_progress: Arc::new(Mutex::new(UpdateProgress::default())),
            app_id: ISAAC_APP_ID,
            auto_update_enabled: load_auto_update().unwrap_or(true),
            auto_update_exclusions: load_auto_update_exclusions().unwrap_or_default(),
            checked_update_paths: HashSet::new(),
            update_selection_touched: false,
            force_update_enabled: false,
            show_log: false,
            language_mode,
            pending_confirmation: None,
            pending_subscribe_notice: None,
            show_force_update_notice: false,
            shown_subscribe_notices: HashSet::new(),
            search_query: String::new(),
            details_cache: Arc::new(Mutex::new(HashMap::new())),
            preview_textures: HashMap::new(),
            preview_failures: HashSet::new(),
            dependency_check: Arc::new(Mutex::new(DependencyCheckState::NotRun)),
            show_dependency_check: false,
        };

        if let Some(path) = load_config() {
            app.game_path = Some(path);
        } else if let Some(path) = find_isaac_game_path() {
            app.game_path = Some(path.clone());
            let _ = save_config(&path);
        }

        if app.game_path.is_some() {
            app.refresh_mods();
            if app.auto_update_enabled {
                app.start_auto_update();
            }
        }

        app
    }
}

impl PatcherApp {
    fn refresh_mods(&mut self) {
        let Some(game_path) = &self.game_path else {
            return;
        };
        let mods_path = game_path.join("mods");
        let had_previous_selection = self.selected_mod_index.is_some();
        let previous_selected_path = self
            .selected_mod()
            .map(|installed_mod| installed_mod.path.clone());
        let previous_workshop_id = self.selected_workshop_id();

        self.state = AppState::Checking;
        self.target_mod_path = None;
        self.selected_mod_index = None;
        self.available_mods.clear();

        if !mods_path.exists() {
            self.status_message = self.t("mods_folder_missing").to_string();
            self.state = AppState::Idle;
            return;
        }

        let steam_roots = self.steam_library_roots();
        self.available_mods = scan_installed_mods(&mods_path, self.app_id, &steam_roots);
        self.sync_checked_update_selection();
        let restored_selection = previous_selected_path
            .as_ref()
            .and_then(|path| {
                self.available_mods
                    .iter()
                    .position(|installed_mod| &installed_mod.path == path)
            })
            .or_else(|| {
                previous_workshop_id.and_then(|workshop_id| {
                    self.available_mods
                        .iter()
                        .position(|installed_mod| installed_mod.workshop_id == Some(workshop_id))
                })
            });
        self.selected_mod_index = restored_selection.or_else(|| {
            if had_previous_selection {
                None
            } else {
                self.available_mods
                    .iter()
                    .position(|installed_mod| installed_mod.workshop_id.is_some())
                    .or_else(|| (!self.available_mods.is_empty()).then_some(0))
            }
        });
        self.apply_selected_mod();

        if self.available_mods.is_empty() {
            self.status_message = self.t("no_installed_mods").to_string();
        } else if self.target_mod_path.is_none() {
            self.status_message = self.t("no_workshop_linked_mods").to_string();
        }

        self.state = AppState::Idle;
    }

    fn selected_mod(&self) -> Option<&InstalledMod> {
        self.selected_mod_index
            .and_then(|index| self.available_mods.get(index))
    }

    fn apply_selected_mod(&mut self) {
        let Some(selected) = self.selected_mod() else {
            self.target_mod_path = None;
            return;
        };

        let target_path = selected.workshop_id.map(|_| selected.path.clone());
        let status_message = status_sentence(selected, self.language());

        if let Some(path) = target_path {
            self.target_mod_path = Some(path);
        } else {
            self.target_mod_path = None;
        }
        self.status_message = status_message;
    }

    fn selected_workshop_id(&self) -> Option<u64> {
        self.selected_mod()?.workshop_id
    }

    fn can_start_update(&self) -> bool {
        self.target_mod_path.is_some()
            && self.selected_workshop_id().is_some()
            && !matches!(self.state, AppState::Syncing)
    }

    fn language(&self) -> UiLanguage {
        match self.language_mode {
            LanguageMode::English => UiLanguage::English,
            LanguageMode::Korean => UiLanguage::Korean,
            LanguageMode::System => system_language(),
        }
    }

    fn t(&self, key: &'static str) -> &'static str {
        tr(self.language(), key)
    }

    fn ensure_selected_details_requested(&mut self) {
        let Some(workshop_id) = self.selected_workshop_id() else {
            return;
        };

        {
            let Ok(cache) = self.details_cache.lock() else {
                return;
            };
            if cache.contains_key(&workshop_id) {
                return;
            }
        }

        if let Ok(mut cache) = self.details_cache.lock() {
            cache.insert(workshop_id, WorkshopDetailsState::Loading);
        }

        let cache = self.details_cache.clone();
        thread::spawn(move || {
            let result = fetch_workshop_details(workshop_id)
                .map(WorkshopDetailsState::Ready)
                .unwrap_or_else(|error| WorkshopDetailsState::Error(error.to_string()));

            if let Ok(mut cache) = cache.lock() {
                cache.insert(workshop_id, result);
            }
        });
    }

    fn retry_selected_details(&mut self) {
        if let Some(workshop_id) = self.selected_workshop_id() {
            if let Ok(mut cache) = self.details_cache.lock() {
                cache.remove(&workshop_id);
            }
            self.preview_textures.remove(&workshop_id);
            self.preview_failures.remove(&workshop_id);
            self.ensure_selected_details_requested();
        }
    }

    fn open_dependency_check(&mut self) {
        if self.dependency_check_is_checking() {
            self.show_dependency_check = true;
        } else {
            self.start_dependency_check(false);
        }
    }

    fn start_dependency_check(&mut self, install_steamcmd: bool) {
        self.show_dependency_check = true;
        if let Ok(mut state) = self.dependency_check.lock() {
            *state = DependencyCheckState::Checking;
        }

        let game_path = self.game_path.clone();
        let state = self.dependency_check.clone();
        thread::spawn(move || {
            let report = run_dependency_check(game_path, install_steamcmd);
            if let Ok(mut state) = state.lock() {
                *state = DependencyCheckState::Ready(report);
            }
        });
    }

    fn dependency_check_is_checking(&self) -> bool {
        self.dependency_check
            .lock()
            .map(|state| matches!(&*state, DependencyCheckState::Checking))
            .unwrap_or(false)
    }

    fn start_patching(&mut self) {
        let Some(index) = self.selected_mod_index else {
            self.status_message = self.t("select_workshop_mod").to_string();
            return;
        };
        self.request_update_indices(vec![index], false, self.force_update_enabled);
    }

    fn start_auto_update(&mut self) {
        let indices = self.auto_update_indices();
        if !indices.is_empty() {
            self.request_update_indices(indices, false, false);
        }
    }

    fn request_update_indices(
        &mut self,
        indices: Vec<usize>,
        confirmed_local_newer: bool,
        force_update: bool,
    ) {
        let indices = self.valid_update_indices(indices);
        if indices.is_empty() {
            self.status_message = self.t("no_updates").to_string();
            return;
        }

        if !confirmed_local_newer
            && indices.iter().any(|index| {
                self.available_mods
                    .get(*index)
                    .is_some_and(|installed_mod| {
                        installed_mod.update_status == ModUpdateStatus::LocalNewer
                    })
            })
        {
            self.pending_confirmation = Some(PendingConfirmation {
                indices,
                force_update,
            });
            return;
        }

        self.start_patching_indices(indices, confirmed_local_newer, force_update);
    }

    fn valid_update_indices(&self, indices: Vec<usize>) -> Vec<usize> {
        indices
            .into_iter()
            .filter(|index| {
                self.available_mods
                    .get(*index)
                    .and_then(|installed_mod| installed_mod.workshop_id)
                    .and_then(valid_workshop_id)
                    .is_some()
            })
            .collect()
    }

    fn update_all_indices(&self, force_update: bool) -> Vec<usize> {
        self.checked_update_indices()
            .into_iter()
            .filter(|index| {
                force_update
                    || self
                        .available_mods
                        .get(*index)
                        .is_some_and(|installed_mod| {
                            installed_mod.update_status.is_update_candidate()
                        })
            })
            .collect()
    }

    fn checked_update_indices(&self) -> Vec<usize> {
        self.available_mods
            .iter()
            .enumerate()
            .filter_map(|(index, installed_mod)| {
                self.can_batch_update_mod(installed_mod)
                    .then_some(())
                    .and_then(|_| {
                        self.checked_update_paths
                            .contains(&installed_mod.path)
                            .then_some(index)
                    })
            })
            .collect()
    }

    fn can_batch_update_mod(&self, installed_mod: &InstalledMod) -> bool {
        let Some(workshop_id) = installed_mod.workshop_id.and_then(valid_workshop_id) else {
            return false;
        };
        !self.auto_update_exclusions.contains(&workshop_id)
    }

    fn sync_checked_update_selection(&mut self) {
        let eligible_paths = self
            .available_mods
            .iter()
            .filter(|installed_mod| self.can_batch_update_mod(installed_mod))
            .map(|installed_mod| installed_mod.path.clone())
            .collect::<HashSet<_>>();

        self.checked_update_paths
            .retain(|path| eligible_paths.contains(path));

        if !self.update_selection_touched {
            self.checked_update_paths = eligible_paths;
        }
    }

    fn auto_update_indices(&self) -> Vec<usize> {
        self.available_mods
            .iter()
            .enumerate()
            .filter_map(|(index, installed_mod)| {
                let workshop_id = valid_workshop_id(installed_mod.workshop_id?)?;
                (installed_mod.update_status.is_update_candidate()
                    && !self.auto_update_exclusions.contains(&workshop_id))
                .then_some(index)
            })
            .collect()
    }

    fn is_auto_update_excluded(&self, workshop_id: u64) -> bool {
        self.auto_update_exclusions.contains(&workshop_id)
    }

    fn set_auto_update_excluded(&mut self, workshop_id: u64, excluded: bool) {
        if excluded {
            self.auto_update_exclusions.insert(workshop_id);
            for installed_mod in &self.available_mods {
                if installed_mod.workshop_id == Some(workshop_id) {
                    self.checked_update_paths.remove(&installed_mod.path);
                }
            }
        } else {
            self.auto_update_exclusions.remove(&workshop_id);
        }
        let _ = save_auto_update_exclusions(&self.auto_update_exclusions);
    }

    fn start_patching_indices(
        &mut self,
        indices: Vec<usize>,
        allow_downgrade: bool,
        force_update: bool,
    ) {
        let mut groups: Vec<UpdateGroup> = Vec::new();
        for index in indices {
            let Some(installed_mod) = self.available_mods.get(index) else {
                continue;
            };
            let Some(workshop_id) = installed_mod.workshop_id.and_then(valid_workshop_id) else {
                continue;
            };
            let target = UpdateTarget {
                path: installed_mod.path.clone(),
                workshop_id,
                display_name: installed_mod.display_name().to_string(),
            };

            if let Some(group) = groups
                .iter_mut()
                .find(|group| group.workshop_id == workshop_id)
            {
                group.targets.push(target);
            } else {
                groups.push(UpdateGroup {
                    workshop_id,
                    targets: vec![target],
                });
            }
        }

        let target_count = groups
            .iter()
            .map(|group| group.targets.len())
            .sum::<usize>();
        let group_count = groups.len();

        if target_count == 0 {
            self.status_message = self.t("no_updates").to_string();
            return;
        }

        let log = self.progress_log.clone();
        let update_progress = self.update_progress.clone();
        let app_id = self.app_id;
        let steam_library_roots = self.steam_library_roots();
        let steam_client_wait = if group_count > 1 || target_count > 1 {
            Duration::from_secs(BULK_STEAM_CLIENT_WAIT_SECS)
        } else {
            Duration::from_secs(SINGLE_STEAM_CLIENT_WAIT_SECS)
        };

        self.state = AppState::Syncing;
        self.status_message = if target_count == 1 {
            self.t("updating_selected").to_string()
        } else {
            format!("{} {}", self.t("updating_all"), target_count)
        };
        if let Ok(mut l) = self.progress_log.lock() {
            l.clear();
            l.push(format!("Update count: {}", target_count));
            l.push(format!("Unique Workshop items: {}", group_count));
            if force_update {
                l.push("Force update enabled: all files will be verified.".to_string());
            }
            l.push("Running updates asynchronously.".to_string());
        }
        reset_update_progress(&update_progress, target_count);

        thread::spawn(move || {
            let (result_tx, result_rx) = mpsc::channel();
            let steamcmd_lock = Arc::new(Mutex::new(()));
            for (group_index, group) in groups.into_iter().enumerate() {
                let log = log.clone();
                let result_tx = result_tx.clone();
                let steam_library_roots = steam_library_roots.clone();
                let steamcmd_lock = steamcmd_lock.clone();
                let update_progress = update_progress.clone();

                thread::spawn(move || {
                    let group_target_count = group.targets.len();
                    if let Ok(mut l) = log.lock() {
                        l.push(format!(
                            "Workshop group [{}/{}]: {} -> {} folder(s)",
                            group_index + 1,
                            group_count,
                            group.workshop_id,
                            group_target_count
                        ));
                    }
                    set_update_progress(
                        &update_progress,
                        format!("Workshop {}", group.workshop_id),
                        5.0,
                        "Downloading workshop content",
                    );

                    let client = SteamWorkshopClient::new(app_id, group.workshop_id)
                        .with_steam_library_roots(steam_library_roots)
                        .with_steam_client_download_wait(steam_client_wait)
                        .with_steamcmd_lock(steamcmd_lock)
                        .with_force_download(force_update);

                    let download_log = log.clone();
                    let download_label = format!("Workshop {}", group.workshop_id);
                    let download_logger = move |msg: String| {
                        if let Ok(mut l) = download_log.lock() {
                            l.push(format!("{}: {}", download_label, msg));
                        }
                    };

                    let source_path = match client.download_latest(Some(&download_logger)) {
                        Ok(source_path) => source_path,
                        Err(error) => {
                            if let Ok(mut l) = log.lock() {
                                l.push(format!("Workshop {}: Error: {}", group.workshop_id, error));
                            }
                            let _ = result_tx.send((group_target_count, true));
                            return;
                        }
                    };
                    set_update_progress(
                        &update_progress,
                        format!("Workshop {}", group.workshop_id),
                        25.0,
                        "Workshop content ready",
                    );

                    for target in group.targets {
                        if let Ok(mut l) = log.lock() {
                            l.push(format!(
                                "{}: Applying Workshop {} to {}",
                                target.display_name,
                                target.workshop_id,
                                target.path.to_string_lossy()
                            ));
                        }

                        let patcher = Patcher::new(client.clone(), target.path)
                            .allow_downgrade(allow_downgrade)
                            .force_update(force_update);
                        let log_for_logger = log.clone();
                        let display_name = target.display_name.clone();
                        let logger = move |msg: String| {
                            if let Ok(mut l) = log_for_logger.lock() {
                                l.push(format!("{}: {}", display_name, msg));
                            }
                        };
                        let progress_for_target = update_progress.clone();
                        let progress_name = target.display_name.clone();
                        let progress = move |percent: f32, detail: String| {
                            set_update_progress(
                                &progress_for_target,
                                progress_name.clone(),
                                percent,
                                detail,
                            );
                        };

                        let had_error = if let Err(error) = patcher
                            .sync_from_source_dir_with_progress(
                                &source_path,
                                Some(logger),
                                Some(progress),
                            ) {
                            if let Ok(mut l) = log.lock() {
                                l.push(format!("{}: Error: {}", target.display_name, error));
                            }
                            true
                        } else {
                            false
                        };

                        let _ = result_tx.send((1, had_error));
                    }
                });
            }
            drop(result_tx);

            let mut had_error = false;
            let mut completed_count = 0;
            for (completed_delta, worker_had_error) in result_rx {
                completed_count += completed_delta;
                had_error |= worker_had_error;
                mark_update_completed(&update_progress, completed_count);
                if let Ok(mut l) = log.lock() {
                    l.push(format!(
                        "Completed {}/{} update jobs.",
                        completed_count, target_count
                    ));
                }
            }

            if let Ok(mut l) = log.lock() {
                if had_error {
                    l.push("Error: One or more updates failed.".to_string());
                } else {
                    l.push("Update complete!".to_string());
                }
            }
        });
    }

    fn steam_library_roots(&self) -> Vec<PathBuf> {
        let mut roots = find_steam_library_roots();
        if let Some(game_path) = &self.game_path {
            for ancestor in game_path.ancestors() {
                if ancestor
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.eq_ignore_ascii_case("steamapps"))
                {
                    roots.push(ancestor.to_path_buf());
                    if let Some(parent) = ancestor.parent() {
                        roots.push(parent.to_path_buf());
                    }
                    break;
                }
            }
        }

        roots.sort_by_key(|path| path.to_string_lossy().to_ascii_lowercase());
        roots.dedup_by_key(|path| path.to_string_lossy().to_ascii_lowercase());
        roots
    }

    fn pick_game_folder(&mut self) {
        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
            self.game_path = Some(folder.clone());
            self.selected_mod_index = None;
            let _ = save_config(&folder);
            self.refresh_mods();
            if self.auto_update_enabled {
                self.start_auto_update();
            }
        }
    }

    fn render_top_bar(&mut self, ui: &mut egui::Ui) {
        let language = self.language();
        let game_folder_label = self.t("game_folder");
        let environment_label = self.t("environment");
        let auto_update_label = self.t("auto_update");
        let show_log_label = self.t("show_log");
        let language_label = self.t("language");
        let path_label = self.t("path");
        let not_selected_label = self.t("not_selected");
        let status_label = self.t("status");
        ui.horizontal_wrapped(|ui| {
            ui.heading(APP_TITLE);
            if ui.button(game_folder_label).clicked() {
                self.pick_game_folder();
            }
            if ui.button(environment_label).clicked() {
                self.open_dependency_check();
            }
            if ui
                .checkbox(&mut self.auto_update_enabled, auto_update_label)
                .changed()
            {
                let _ = save_auto_update(self.auto_update_enabled);
            }
            ui.checkbox(&mut self.show_log, show_log_label);
            ui.label(language_label);
            egui::ComboBox::from_id_source("language_mode")
                .selected_text(self.language_mode.label(language))
                .show_ui(ui, |ui| {
                    for mode in [
                        LanguageMode::System,
                        LanguageMode::English,
                        LanguageMode::Korean,
                    ] {
                        if ui
                            .selectable_value(&mut self.language_mode, mode, mode.label(language))
                            .changed()
                        {
                            let _ = save_language_mode(self.language_mode);
                        }
                    }
                });
        });

        egui::Grid::new("top_status_grid")
            .num_columns(2)
            .spacing([10.0, 5.0])
            .show(ui, |ui| {
                ui.label(path_label);
                if let Some(path) = &self.game_path {
                    ui.add(egui::Label::new(path.to_string_lossy()).wrap(true));
                } else {
                    ui.colored_label(egui::Color32::from_rgb(200, 80, 80), not_selected_label);
                }
                ui.end_row();

                ui.label(status_label);
                ui.add(egui::Label::new(self.current_status_text()).wrap(true));
                ui.end_row();

                if self.should_show_update_progress() {
                    ui.label(self.t("progress"));
                    self.render_update_progress(ui);
                    ui.end_row();
                }
            });
    }

    fn current_status_text(&self) -> String {
        if matches!(
            self.state,
            AppState::Syncing | AppState::Done | AppState::Error
        ) {
            return self.status_message.clone();
        }

        let Some(selected) = self.selected_mod() else {
            return self.status_message.clone();
        };

        status_sentence(selected, self.language())
    }

    fn should_show_update_progress(&self) -> bool {
        self.update_progress
            .lock()
            .map(|progress| progress.total > 0)
            .unwrap_or(false)
    }

    fn render_update_progress(&self, ui: &mut egui::Ui) {
        let progress = self
            .update_progress
            .lock()
            .map(|progress| progress.clone())
            .unwrap_or_default();
        if progress.total == 0 {
            return;
        }

        let total_fraction = (progress.completed as f32 / progress.total as f32).clamp(0.0, 1.0);
        ui.vertical(|ui| {
            ui.add(egui::ProgressBar::new(total_fraction).text(format!(
                "{}: {}/{} ({:.0}%)",
                self.t("overall_progress"),
                progress.completed,
                progress.total,
                total_fraction * 100.0
            )));

            if let Some(current_mod) = progress.current_mod.as_deref() {
                let current_fraction = (progress.current_percent / 100.0).clamp(0.0, 1.0);
                let detail = progress.current_detail.as_deref().unwrap_or_default();
                ui.add(egui::ProgressBar::new(current_fraction).text(format!(
                    "{}: {} {:.0}% {}",
                    self.t("current_mod_progress"),
                    current_mod,
                    progress.current_percent,
                    detail
                )));
            }
        });
    }

    fn render_mod_browser(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let language = self.language();
        let installed_mods_label = self.t("installed_mods");
        let refresh_mods_label = self.t("refresh_mods");
        let search_label = self.t("search");
        let search_hint = self.t("search_hint");
        let no_mods_label = self.t("no_mods");
        let no_match_label = self.t("no_match");
        ui.horizontal_wrapped(|ui| {
            ui.label(installed_mods_label);
            if ui.button(refresh_mods_label).clicked() {
                self.refresh_mods();
            }
            ui.add_space(10.0);
            ui.label(search_label);
            ui.add(
                egui::TextEdit::singleline(&mut self.search_query)
                    .desired_width(280.0)
                    .hint_text(search_hint),
            );
        });

        let list_width = (ui.available_width() * 0.40).clamp(320.0, 480.0);
        let browser_height = ui.available_height().max(240.0);
        let visible_indices = self.filtered_mod_indices();
        let mut clicked_mod_index = None;

        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(list_width);
                ui.set_min_height(browser_height);

                egui::ScrollArea::vertical()
                    .id_source("installed_mods_scroll")
                    .max_height(browser_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if self.available_mods.is_empty() {
                            ui.label(no_mods_label);
                        } else if visible_indices.is_empty() {
                            ui.label(no_match_label);
                        }

                        for index in &visible_indices {
                            let installed_mod = &self.available_mods[*index];
                            let selected = self.selected_mod_index == Some(*index);
                            let path = installed_mod.path.clone();
                            let can_batch_update = self.can_batch_update_mod(installed_mod);
                            let mut label = installed_mod.row_label(language);
                            if installed_mod.workshop_id.is_some_and(|workshop_id| {
                                self.is_auto_update_excluded(workshop_id)
                            }) {
                                label.push_str(" | ");
                                label.push_str(tr(language, "auto_excluded_short"));
                            }
                            let text = egui::RichText::new(label)
                                .color(installed_mod.update_status.color());
                            ui.horizontal(|ui| {
                                let mut checked = self.checked_update_paths.contains(&path);
                                let checkbox_response = ui.add_enabled(
                                    can_batch_update,
                                    egui::Checkbox::without_text(&mut checked),
                                );
                                if checkbox_response.changed() {
                                    self.update_selection_touched = true;
                                    if checked {
                                        self.checked_update_paths.insert(path.clone());
                                    } else {
                                        self.checked_update_paths.remove(&path);
                                    }
                                }

                                if ui.selectable_label(selected, text).clicked() {
                                    clicked_mod_index = Some(*index);
                                }
                            });
                        }
                    });
            });

            ui.separator();

            ui.vertical(|ui| {
                ui.set_min_width((ui.available_width()).max(260.0));
                ui.set_height(browser_height);
                egui::ScrollArea::vertical()
                    .id_source("selected_mod_details_scroll")
                    .max_height(browser_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_min_width((ui.available_width()).max(260.0));
                        self.render_selected_mod_details(ui, ctx, browser_height);
                    });
            });
        });

        if let Some(index) = clicked_mod_index {
            self.selected_mod_index = Some(index);
            if !matches!(self.state, AppState::Syncing) {
                self.state = AppState::Idle;
            }
            self.apply_selected_mod();
            self.ensure_selected_details_requested();
        }
    }

    fn filtered_mod_indices(&self) -> Vec<usize> {
        let query = self.search_query.trim().to_ascii_lowercase();
        self.available_mods
            .iter()
            .enumerate()
            .filter_map(|(index, installed_mod)| {
                (query.is_empty() || mod_matches_query(installed_mod, &query)).then_some(index)
            })
            .collect()
    }

    fn render_selected_mod_details(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        max_height: f32,
    ) {
        let language = self.language();
        let Some(selected) = self.selected_mod().cloned() else {
            ui.label(self.t("select_mod"));
            return;
        };

        let detail_start_y = ui.cursor().top();
        ui.heading(selected.display_name());
        egui::Grid::new("selected_mod_local_details")
            .num_columns(2)
            .spacing([10.0, 6.0])
            .show(ui, |ui| {
                ui.label(self.t("folder"));
                ui.label(&selected.folder_name);
                ui.end_row();

                ui.label(self.t("local_version"));
                ui.label(selected.version_label());
                ui.end_row();

                ui.label(self.t("steam_version"));
                ui.label(selected.steam_version.as_deref().unwrap_or("unknown"));
                ui.end_row();

                if let Some(timestamp) = selected.steam_updated_at {
                    ui.label(self.t("steam_updated"));
                    ui.label(format_timestamp(Some(timestamp)));
                    ui.end_row();
                }

                ui.label(self.t("version_status"));
                ui.colored_label(
                    selected.update_status.color(),
                    selected.update_status.label(language),
                );
                ui.end_row();

                if let Some(author) = &selected.author {
                    ui.label(self.t("author"));
                    ui.label(author);
                    ui.end_row();
                }

                ui.label(self.t("workshop_id"));
                if let Some(workshop_id) = selected.workshop_id {
                    ui.label(workshop_id.to_string());
                } else {
                    ui.colored_label(egui::Color32::from_rgb(230, 150, 50), self.t("local_only"));
                }
                ui.end_row();

                if let Some(workshop_id) = selected.workshop_id {
                    ui.label(self.t("auto_update"));
                    let mut excluded = self.is_auto_update_excluded(workshop_id);
                    if ui
                        .checkbox(&mut excluded, self.t("exclude_auto_update"))
                        .changed()
                    {
                        self.set_auto_update_excluded(workshop_id, excluded);
                    }
                    ui.end_row();
                }
            });

        ui.add_space(8.0);

        let Some(workshop_id) = selected.workshop_id else {
            if let Some(description) = selected.description.as_deref() {
                ui.label(egui::RichText::new(self.t("description")).strong());
                let used_height = ui.cursor().top() - detail_start_y;
                let description_height =
                    (max_height - used_height - 10.0).max(DESCRIPTION_MIN_HEIGHT);
                render_description_text_box(
                    ui,
                    ("local_description_scroll", selected.folder_name.as_str()),
                    description,
                    description_height,
                );
            } else {
                ui.label(self.t("no_workshop_id_meta"));
            }
            return;
        };

        let details_state = self
            .details_cache
            .lock()
            .ok()
            .and_then(|cache| cache.get(&workshop_id).cloned());

        match details_state {
            Some(WorkshopDetailsState::Ready(details)) => {
                let used_height = ui.cursor().top() - detail_start_y;
                let workshop_height = (max_height - used_height).max(180.0);
                self.render_workshop_details(ui, ctx, &details, workshop_height);
            }
            Some(WorkshopDetailsState::Error(error)) => {
                ui.colored_label(
                    egui::Color32::from_rgb(210, 80, 80),
                    format!("{}: {}", self.t("workshop_details_failed"), error),
                );
                if ui.button(self.t("retry_details")).clicked() {
                    self.retry_selected_details();
                }
                if ui.button(self.t("open_workshop_steam")).clicked() {
                    if let Err(error) = open_workshop_in_steam(workshop_id) {
                        self.status_message =
                            format!("{}: {}", self.t("open_workshop_failed"), error);
                    }
                }
            }
            _ => {
                ui.spinner();
                ui.label(self.t("loading_details"));
            }
        }
    }

    fn render_workshop_details(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        details: &WorkshopDetails,
        max_height: f32,
    ) {
        let language = self.language();
        let start_y = ui.cursor().top();
        if let Some(texture) = self.preview_texture(ctx, details).cloned() {
            let texture_size = texture.size();
            let original = egui::vec2(texture_size[0] as f32, texture_size[1] as f32);
            let max_width = ui.available_width().min(360.0);
            let max_preview_height = (max_height * 0.34).clamp(120.0, 240.0);
            let scale = (max_width / original.x).min(max_preview_height / original.y);
            let size = egui::vec2(original.x * scale, original.y * scale);
            ui.add(egui::Image::from_texture(&texture).fit_to_exact_size(size));
            ui.add_space(6.0);
        } else if details.preview_url.is_some() {
            ui.colored_label(
                egui::Color32::from_rgb(150, 150, 150),
                tr(language, "preview_unsupported"),
            );
        }

        ui.label(egui::RichText::new(&details.title).strong());
        egui::Grid::new(("workshop_details_grid", details.workshop_id))
            .num_columns(2)
            .spacing([10.0, 5.0])
            .show(ui, |ui| {
                ui.label(tr(language, "steam_updated"));
                ui.label(format_timestamp(details.time_updated));
                ui.end_row();

                ui.label(tr(language, "created"));
                ui.label(format_timestamp(details.time_created));
                ui.end_row();

                ui.label(tr(language, "size"));
                ui.label(format_bytes(details.file_size));
                ui.end_row();

                ui.label(tr(language, "views"));
                ui.label(format_count(details.views));
                ui.end_row();

                ui.label(tr(language, "subscriptions"));
                ui.label(format_count(details.subscriptions));
                ui.end_row();

                ui.label(tr(language, "favorites"));
                ui.label(format_count(details.favorited));
                ui.end_row();
            });

        self.render_workshop_creators(ui, details, language);
        self.render_workshop_required_items(ui, details, language);
        self.render_workshop_tags(ui, details, language);

        ui.horizontal_wrapped(|ui| {
            if ui.button(tr(language, "open_workshop_steam")).clicked() {
                match open_workshop_in_steam(details.workshop_id) {
                    Ok(()) => {
                        self.status_message = tr(language, "opened_steam").to_string();
                    }
                    Err(error) => {
                        self.status_message =
                            format!("{}: {}", tr(language, "open_workshop_failed"), error);
                    }
                }
            }
            ui.hyperlink_to(
                tr(language, "open_web_page"),
                workshop_url(self.app_id, details.workshop_id),
            );
        });

        ui.add_space(6.0);
        ui.label(egui::RichText::new(tr(language, "description")).strong());
        let used_height = ui.cursor().top() - start_y;
        let remaining_height = (max_height - used_height - 10.0).max(0.0);
        let description_height = remaining_height.max(DESCRIPTION_MIN_HEIGHT);
        render_description_text_box(
            ui,
            ("workshop_description_scroll", details.workshop_id),
            &details.description,
            description_height,
        );
    }

    fn render_workshop_creators(
        &mut self,
        ui: &mut egui::Ui,
        details: &WorkshopDetails,
        language: UiLanguage,
    ) {
        if details.creators.is_empty() {
            return;
        }

        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(if details.creators.len() == 1 {
                tr(language, "creator")
            } else {
                tr(language, "creators")
            })
            .strong(),
        );
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for (index, creator) in details.creators.iter().enumerate() {
                if index > 0 {
                    ui.label(", ");
                }
                let response = ui
                    .link(&creator.name)
                    .on_hover_text(creator.profile_url.as_str());
                if response.clicked() {
                    match open_steam_profile_url(&creator.profile_url) {
                        Ok(()) => {
                            self.status_message = tr(language, "opened_profile").to_string();
                        }
                        Err(error) => {
                            self.status_message =
                                format!("{}: {}", tr(language, "open_profile_failed"), error);
                        }
                    }
                }
            }
        });
    }

    fn render_workshop_tags(
        &mut self,
        ui: &mut egui::Ui,
        details: &WorkshopDetails,
        language: UiLanguage,
    ) {
        if details.tags.is_empty() {
            return;
        }

        ui.add_space(4.0);
        ui.label(egui::RichText::new(tr(language, "tags")).strong());
        ui.add(egui::Label::new(details.tags.join(", ")).wrap(true));
    }

    fn render_workshop_required_items(
        &mut self,
        ui: &mut egui::Ui,
        details: &WorkshopDetails,
        language: UiLanguage,
    ) {
        if details.required_items.is_empty() {
            return;
        }

        ui.add_space(4.0);
        ui.label(egui::RichText::new(tr(language, "required_items")).strong());
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for (index, item) in details.required_items.iter().enumerate() {
                if index > 0 {
                    ui.label(", ");
                }
                let response = ui.link(&item.title).on_hover_text(item.url.as_str());
                if response.clicked() {
                    let result = if let Some(workshop_id) = item.workshop_id {
                        open_workshop_in_steam(workshop_id)
                    } else {
                        open_steam_or_web(&item.url)
                    };
                    match result {
                        Ok(()) => {
                            self.status_message = tr(language, "opened_steam").to_string();
                        }
                        Err(error) => {
                            self.status_message =
                                format!("{}: {}", tr(language, "open_workshop_failed"), error);
                        }
                    }
                }
            }
        });
    }

    fn preview_texture(
        &mut self,
        ctx: &egui::Context,
        details: &WorkshopDetails,
    ) -> Option<&egui::TextureHandle> {
        if self.preview_failures.contains(&details.workshop_id) {
            return None;
        }

        if !self.preview_textures.contains_key(&details.workshop_id) {
            let Some(bytes) = details.preview_image.as_deref() else {
                self.preview_failures.insert(details.workshop_id);
                return None;
            };
            let Ok(image) = image::load_from_memory(bytes) else {
                self.preview_failures.insert(details.workshop_id);
                return None;
            };
            let image = image.to_rgba8();
            let size = [image.width() as usize, image.height() as usize];
            let pixels = image.as_raw();
            let color_image = egui::ColorImage::from_rgba_unmultiplied(size, pixels);
            let texture = ctx.load_texture(
                format!("workshop_preview_{}", details.workshop_id),
                color_image,
                egui::TextureOptions::LINEAR,
            );
            self.preview_textures.insert(details.workshop_id, texture);
        }

        self.preview_textures.get(&details.workshop_id)
    }

    fn render_update_controls(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            if matches!(self.state, AppState::Syncing) {
                ui.spinner();
                ui.label(self.t("downloading_applying"));
            } else {
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            self.can_start_update(),
                            update_action_button(
                                self.t("download_apply"),
                                220.0,
                                self.force_update_enabled,
                            ),
                        )
                        .clicked()
                    {
                        self.start_patching();
                    }

                    let can_update_all = !self.checked_update_indices().is_empty();
                    let update_all_indices = self.update_all_indices(self.force_update_enabled);
                    if ui
                        .add_enabled(
                            can_update_all,
                            update_action_button(
                                self.t("update_all"),
                                160.0,
                                self.force_update_enabled,
                            ),
                        )
                        .clicked()
                    {
                        if update_all_indices.is_empty() {
                            self.status_message = self.t("no_updates").to_string();
                        } else {
                            self.request_update_indices(
                                update_all_indices,
                                false,
                                self.force_update_enabled,
                            );
                        }
                    }

                    let mut force_update_enabled = self.force_update_enabled;
                    if ui
                        .checkbox(&mut force_update_enabled, self.t("force_update"))
                        .changed()
                    {
                        self.force_update_enabled = force_update_enabled;
                        if force_update_enabled {
                            self.show_force_update_notice = true;
                        }
                    }
                });
            }
        });
    }

    fn render_log(&mut self, ui: &mut egui::Ui, height: f32) {
        ui.label(self.t("log"));

        let logs = self.progress_log.lock().unwrap();
        let mut text = logs
            .iter()
            .filter(|log| parse_subscribe_notice_marker(log).is_none())
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        ui.add_sized(
            [ui.available_width(), height],
            egui::TextEdit::multiline(&mut text)
                .id_source("progress_log_text")
                .font(egui::TextStyle::Monospace)
                .desired_rows(8)
                .interactive(true)
                .lock_focus(true)
                .desired_width(f32::INFINITY),
        );
    }

    fn render_confirmation_dialog(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending_confirmation.clone() else {
            return;
        };

        let mut confirm = false;
        let mut cancel = false;
        let language = self.language();
        let count = pending.indices.len();

        egui::Window::new(tr(language, "confirm_downgrade_title"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(if count == 1 {
                    tr(language, "confirm_downgrade_single")
                } else {
                    tr(language, "confirm_downgrade_all")
                });
                ui.add_space(8.0);
                egui::ScrollArea::vertical()
                    .max_height(120.0)
                    .show(ui, |ui| {
                        for index in &pending.indices {
                            if let Some(installed_mod) = self.available_mods.get(*index) {
                                if installed_mod.update_status == ModUpdateStatus::LocalNewer {
                                    ui.label(format!(
                                        "{}: local {}, Steam {}",
                                        installed_mod.display_name(),
                                        installed_mod.version_label(),
                                        installed_mod.steam_version.as_deref().unwrap_or("unknown")
                                    ));
                                }
                            }
                        }
                    });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(tr(language, "cancel")).clicked() {
                        cancel = true;
                    }
                    if ui.button(tr(language, "match_steam_version")).clicked() {
                        confirm = true;
                    }
                });
            });

        if cancel {
            self.pending_confirmation = None;
        } else if confirm {
            self.pending_confirmation = None;
            self.request_update_indices(pending.indices, true, pending.force_update);
        }
    }

    fn render_subscribe_notice_dialog(&mut self, ctx: &egui::Context) {
        let Some(notice) = self.pending_subscribe_notice.clone() else {
            return;
        };

        let mut close = false;
        let language = self.language();
        egui::Window::new(tr(language, "subscribe_required_title"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(tr(language, "subscribe_required_body"));
                ui.add_space(8.0);
                ui.label(format!("Workshop ID: {}", notice.workshop_id));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(tr(language, "open_workshop_steam")).clicked() {
                        match open_workshop_in_steam(notice.workshop_id) {
                            Ok(()) => {
                                self.status_message = tr(language, "opened_steam").to_string();
                            }
                            Err(error) => {
                                self.status_message =
                                    format!("{}: {}", tr(language, "open_workshop_failed"), error);
                            }
                        }
                    }
                    if ui.button(tr(language, "ok")).clicked() {
                        close = true;
                    }
                });
            });

        if close {
            self.pending_subscribe_notice = None;
        }
    }

    fn render_force_update_notice_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_force_update_notice {
            return;
        }

        let mut close = false;
        let language = self.language();
        egui::Window::new(tr(language, "force_update_title"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(tr(language, "force_update_body"));
                ui.add_space(8.0);
                if ui.button(tr(language, "ok")).clicked() {
                    close = true;
                }
            });

        if close {
            self.show_force_update_notice = false;
        }
    }

    fn render_dependency_check_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_dependency_check {
            return;
        }

        let language = self.language();
        let state = self
            .dependency_check
            .lock()
            .map(|state| state.clone())
            .unwrap_or_else(|_| {
                DependencyCheckState::Error("Dependency check state is unavailable".to_string())
            });
        let is_checking = matches!(&state, DependencyCheckState::Checking);
        let mut window_open = true;
        let mut close = false;
        let mut refresh = false;
        let mut prepare = false;

        egui::Window::new(tr(language, "environment_check"))
            .collapsible(false)
            .resizable(true)
            .default_width(720.0)
            .open(&mut window_open)
            .show(ctx, |ui| {
                match &state {
                    DependencyCheckState::NotRun => {
                        ui.label(tr(language, "environment_not_checked"));
                    }
                    DependencyCheckState::Checking => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(tr(language, "checking_environment"));
                        });
                    }
                    DependencyCheckState::Ready(report) => {
                        self.render_dependency_report(ui, report);
                    }
                    DependencyCheckState::Error(error) => {
                        ui.colored_label(
                            egui::Color32::from_rgb(210, 80, 80),
                            format!("{}: {}", tr(language, "error"), error),
                        );
                    }
                }

                ui.add_space(10.0);
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(!is_checking, egui::Button::new(tr(language, "refresh")))
                        .clicked()
                    {
                        refresh = true;
                    }
                    if ui
                        .add_enabled(
                            !is_checking,
                            egui::Button::new(tr(language, "prepare_steamcmd")),
                        )
                        .clicked()
                    {
                        prepare = true;
                    }
                    if ui.button(tr(language, "close")).clicked() {
                        close = true;
                    }
                });
            });

        if close || !window_open {
            self.show_dependency_check = false;
        } else if prepare {
            self.start_dependency_check(true);
        } else if refresh {
            self.start_dependency_check(false);
        }
    }

    fn render_dependency_report(&self, ui: &mut egui::Ui, report: &DependencyReport) {
        let language = self.language();
        ui.label(tr(language, "environment_check_body"));
        ui.add_space(6.0);

        egui::Grid::new("dependency_report_grid")
            .num_columns(3)
            .spacing([10.0, 6.0])
            .show(ui, |ui| {
                dependency_row(
                    ui,
                    tr(language, "steam_client"),
                    report.steam_path.is_some(),
                    path_or_missing(report.steam_path.as_ref(), tr(language, "not_found")),
                    language,
                );
                dependency_row(
                    ui,
                    tr(language, "isaac_game"),
                    report.isaac_path.is_some(),
                    path_or_missing(report.isaac_path.as_ref(), tr(language, "not_selected")),
                    language,
                );
                dependency_row(
                    ui,
                    tr(language, "steam_libraries"),
                    !report.steam_library_roots.is_empty(),
                    report.steam_library_roots.len().to_string(),
                    language,
                );
                dependency_row(
                    ui,
                    tr(language, "workshop_cache"),
                    report.workshop_cache_roots > 0,
                    format!(
                        "{}/{}",
                        report.workshop_cache_roots,
                        report.steam_library_roots.len()
                    ),
                    language,
                );
                dependency_row(
                    ui,
                    tr(language, "steamcmd"),
                    report.steamcmd_path.is_some() && report.steamcmd_error.is_none(),
                    path_or_missing(report.steamcmd_path.as_ref(), tr(language, "not_installed")),
                    language,
                );
                dependency_row(
                    ui,
                    tr(language, "steam_web_api"),
                    report.steam_web_api_error.is_none(),
                    report
                        .steam_web_api_error
                        .as_deref()
                        .unwrap_or(tr(language, "reachable"))
                        .to_string(),
                    language,
                );
            });

        if !report.steam_library_roots.is_empty() {
            ui.add_space(6.0);
            egui::CollapsingHeader::new(tr(language, "steam_library_paths"))
                .default_open(false)
                .show(ui, |ui| {
                    for path in &report.steam_library_roots {
                        ui.add(egui::Label::new(path.to_string_lossy()).wrap(true));
                    }
                });
        }

        if let Some(error) = &report.steamcmd_error {
            ui.add_space(6.0);
            ui.colored_label(
                egui::Color32::from_rgb(210, 80, 80),
                format!("{}: {}", tr(language, "steamcmd_prepare_failed"), error),
            );
        }

        ui.add_space(8.0);
        ui.add(
            egui::Label::new(
                egui::RichText::new(tr(language, "environment_note"))
                    .color(egui::Color32::from_rgb(130, 130, 130)),
            )
            .wrap(true),
        );
    }

    fn sync_state_from_logs(&mut self) {
        let logs = self.progress_log.lock().ok().map(|logs| logs.clone());
        let Some(logs) = logs else {
            return;
        };

        self.sync_subscribe_notice_from_logs(&logs);

        if !matches!(self.state, AppState::Syncing) {
            return;
        }

        let Some(last) = logs.last() else {
            return;
        };

        if last == "Update complete!" {
            self.state = AppState::Done;
            self.pending_subscribe_notice = None;
            self.refresh_mods();
            self.state = AppState::Done;
            self.status_message = self.t("update_success").to_string();
        } else if last == "Error: One or more updates failed." {
            self.state = AppState::Error;
            self.status_message = self.t("update_failed").to_string();
        }
    }

    fn sync_subscribe_notice_from_logs(&mut self, logs: &[String]) {
        for log in logs {
            if let Some(workshop_id) = parse_subscribe_notice_marker(log) {
                if self.shown_subscribe_notices.insert(workshop_id) {
                    self.pending_subscribe_notice = Some(PendingSubscribeNotice { workshop_id });
                    break;
                }
            }
        }
    }

    fn ensure_buttons_visible_viewport(&self, ctx: &egui::Context) {
        let current_size = ctx.input(|input| input.screen_rect().size());
        let target_size = egui::vec2(
            current_size.x.max(MIN_VISIBLE_WIDTH),
            current_size.y.max(MIN_VISIBLE_HEIGHT),
        );

        if target_size != current_size {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(target_size));
        }
    }
}

impl eframe::App for PatcherApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ensure_buttons_visible_viewport(ctx);

        if matches!(self.state, AppState::Syncing) {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        if self.show_dependency_check && self.dependency_check_is_checking() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        self.sync_state_from_logs();
        self.ensure_selected_details_requested();
        if self.selected_workshop_id().is_some_and(|workshop_id| {
            self.details_cache
                .lock()
                .ok()
                .and_then(|cache| cache.get(&workshop_id).cloned())
                .is_some_and(|state| matches!(state, WorkshopDetailsState::Loading))
        }) {
            ctx.request_repaint_after(Duration::from_millis(250));
        }

        egui::TopBottomPanel::bottom("actions_panel")
            .resizable(false)
            .exact_height(ACTIONS_PANEL_HEIGHT)
            .show(ctx, |ui| {
                ui.add_space(6.0);
                self.render_update_controls(ui);
                ui.add_space(6.0);
            });

        if self.show_log {
            egui::TopBottomPanel::bottom("log_panel")
                .resizable(true)
                .default_height(LOG_PANEL_DEFAULT_HEIGHT)
                .height_range(LOG_PANEL_MIN_HEIGHT..=LOG_PANEL_MAX_HEIGHT)
                .show(ctx, |ui| {
                    ui.add_space(4.0);
                    let log_height = (ui.available_height() - 24.0)
                        .clamp(LOG_PANEL_MIN_HEIGHT - 24.0, LOG_PANEL_MAX_HEIGHT - 24.0);
                    self.render_log(ui, log_height);
                    ui.add_space(4.0);
                });
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            self.render_top_bar(ui);
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(8.0);

            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), ui.available_height()),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    self.render_mod_browser(ui, ctx);
                },
            );
        });

        self.render_confirmation_dialog(ctx);
        self.render_subscribe_notice_dialog(ctx);
        self.render_force_update_notice_dialog(ctx);
        self.render_dependency_check_dialog(ctx);
    }
}

pub fn run() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_TITLE)
            .with_inner_size([1180.0, 860.0])
            .with_min_inner_size([MIN_VISIBLE_WIDTH, MIN_VISIBLE_HEIGHT])
            .with_resizable(true),
        ..Default::default()
    };
    eframe::run_native(
        APP_TITLE,
        options,
        Box::new(|cc| {
            install_system_fonts(&cc.egui_ctx);

            let mut style = (*cc.egui_ctx.style()).clone();
            style.spacing.item_spacing = egui::vec2(8.0, 8.0);
            style.visuals.widgets.inactive.rounding = egui::Rounding::same(4.0);
            style.visuals.widgets.active.rounding = egui::Rounding::same(4.0);
            style.visuals.widgets.hovered.rounding = egui::Rounding::same(4.0);
            for (_, font_id) in style.text_styles.iter_mut() {
                font_id.size *= 1.1;
            }
            cc.egui_ctx.set_style(style);

            Box::new(PatcherApp::default())
        }),
    )
}

fn install_system_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let mut loaded_font_names = Vec::new();

    fonts.font_data.insert(
        "embedded_noto_sans_cjk_kr".to_string(),
        egui::FontData::from_static(include_bytes!("../assets/fonts/NotoSansCJKkr-Regular.otf")),
    );
    loaded_font_names.push("embedded_noto_sans_cjk_kr".to_string());

    for path in system_font_candidates() {
        let Ok(bytes) = fs::read(path) else {
            continue;
        };
        let name = format!("system_fallback_{}", loaded_font_names.len());
        fonts
            .font_data
            .insert(name.clone(), egui::FontData::from_owned(bytes));
        loaded_font_names.push(name);
    }

    if loaded_font_names.is_empty() {
        return;
    }

    for font_name in loaded_font_names {
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .push(font_name.clone());
        fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .push(font_name);
    }

    ctx.set_fonts(fonts);
}

fn system_font_candidates() -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        return [
            r"C:\Windows\Fonts\NotoSansKR-VF.ttf",
            r"C:\Windows\Fonts\NotoSerifKR-VF.ttf",
            r"C:\Windows\Fonts\malgun.ttf",
            r"C:\Windows\Fonts\malgunbd.ttf",
            r"C:\Windows\Fonts\NGULIM.TTF",
            r"C:\Windows\Fonts\GOTHIC.TTF",
            r"C:\Windows\Fonts\SimsunExtG.ttf",
            r"C:\Windows\Fonts\simsunb.ttf",
            r"C:\Windows\Fonts\gulim.ttc",
            r"C:\Windows\Fonts\msgothic.ttc",
            r"C:\Windows\Fonts\msyh.ttc",
            r"C:\Windows\Fonts\simsun.ttc",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
    }

    #[cfg(target_os = "macos")]
    {
        return [
            "/System/Library/Fonts/AppleSDGothicNeo.ttc",
            "/System/Library/Fonts/AppleGothic.ttf",
            "/System/Library/Fonts/PingFang.ttc",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
    }

    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        [
            "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect()
    }
}

fn run_dependency_check(game_path: Option<PathBuf>, install_steamcmd: bool) -> DependencyReport {
    let steam_path = detect_steam_path();
    let isaac_path = game_path.or_else(find_isaac_game_path);
    let steam_library_roots = find_steam_library_roots();
    let workshop_cache_roots = steam_library_roots
        .iter()
        .filter(|root| has_workshop_cache_root(root))
        .count();

    let mut steamcmd_error = None;
    let steamcmd_path = if install_steamcmd {
        match prepare_steamcmd(None) {
            Ok(path) => Some(path),
            Err(error) => {
                steamcmd_error = Some(error.to_string());
                find_steamcmd()
            }
        }
    } else {
        find_steamcmd()
    };

    let steam_web_api_error = fetch_workshop_summaries(&[CONCH_BLESSING_WORKSHOP_ID])
        .err()
        .map(|error| error.to_string());

    DependencyReport {
        steam_path,
        isaac_path,
        steam_library_roots,
        workshop_cache_roots,
        steamcmd_path,
        steamcmd_error,
        steam_web_api_error,
    }
}

fn detect_steam_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        if let Some(path) = crate::fs_utils::find_steam_path_from_registry() {
            return Some(path);
        }
    }

    crate::fs_utils::find_steam_from_path_env()
}

fn has_workshop_cache_root(root: &Path) -> bool {
    root.join("steamapps").join("workshop").exists() || root.join("workshop").exists()
}

fn dependency_row(ui: &mut egui::Ui, label: &str, ok: bool, value: String, language: UiLanguage) {
    ui.label(label);
    ui.colored_label(
        dependency_status_color(ok),
        dependency_status_label(ok, language),
    );
    ui.add(egui::Label::new(value).wrap(true));
    ui.end_row();
}

fn render_description_text_box(
    ui: &mut egui::Ui,
    id_source: impl std::hash::Hash,
    text: &str,
    height: f32,
) {
    let inner_margin = 8.0;
    let inner_height =
        (height - inner_margin * 2.0).max(DESCRIPTION_MIN_HEIGHT - inner_margin * 2.0);

    egui::Frame::group(ui.style())
        .fill(ui.visuals().extreme_bg_color)
        .inner_margin(egui::Margin::same(inner_margin))
        .show(ui, |ui| {
            ui.set_min_height(inner_height);
            egui::ScrollArea::vertical()
                .id_source(id_source)
                .max_height(inner_height)
                .min_scrolled_height(inner_height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add(egui::Label::new(text).wrap(true));
                });
        });
}

fn reset_update_progress(progress: &Arc<Mutex<UpdateProgress>>, total: usize) {
    if let Ok(mut progress) = progress.lock() {
        *progress = UpdateProgress {
            total,
            completed: 0,
            current_mod: None,
            current_detail: None,
            current_percent: 0.0,
        };
    }
}

fn set_update_progress(
    progress: &Arc<Mutex<UpdateProgress>>,
    current_mod: String,
    current_percent: f32,
    current_detail: impl Into<String>,
) {
    if let Ok(mut progress) = progress.lock() {
        progress.current_mod = Some(current_mod);
        progress.current_percent = current_percent.clamp(0.0, 100.0);
        progress.current_detail = Some(current_detail.into());
    }
}

fn mark_update_completed(progress: &Arc<Mutex<UpdateProgress>>, completed: usize) {
    if let Ok(mut progress) = progress.lock() {
        progress.completed = completed.min(progress.total);
        if progress.completed >= progress.total {
            progress.current_percent = 100.0;
            progress.current_detail = Some("Update complete".to_string());
        }
    }
}

fn update_action_button(label: &str, width: f32, force_update: bool) -> egui::Button<'_> {
    let button = if force_update {
        egui::Button::new(egui::RichText::new(label).color(egui::Color32::WHITE))
            .fill(egui::Color32::from_rgb(190, 55, 55))
    } else {
        egui::Button::new(label)
    };

    button.min_size([width, 40.0].into())
}

fn dependency_status_color(ok: bool) -> egui::Color32 {
    if ok {
        egui::Color32::from_rgb(80, 170, 100)
    } else {
        egui::Color32::from_rgb(210, 120, 60)
    }
}

fn dependency_status_label(ok: bool, language: UiLanguage) -> &'static str {
    if ok {
        tr(language, "available")
    } else {
        tr(language, "missing")
    }
}

fn path_or_missing(path: Option<&PathBuf>, missing: &str) -> String {
    path.map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|| missing.to_string())
}

fn mod_matches_query(installed_mod: &InstalledMod, query: &str) -> bool {
    installed_mod
        .display_name()
        .to_ascii_lowercase()
        .contains(query)
        || installed_mod
            .folder_name
            .to_ascii_lowercase()
            .contains(query)
        || installed_mod
            .version
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .contains(query)
        || installed_mod
            .description
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .contains(query)
        || installed_mod
            .author
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .contains(query)
        || installed_mod
            .workshop_id
            .map(|workshop_id| workshop_id.to_string().contains(query))
            .unwrap_or(false)
}

fn system_language() -> UiLanguage {
    sys_locale::get_locale()
        .map(|locale| locale.to_ascii_lowercase())
        .filter(|locale| locale.starts_with("ko"))
        .map(|_| UiLanguage::Korean)
        .unwrap_or(UiLanguage::English)
}

fn status_sentence(installed_mod: &InstalledMod, language: UiLanguage) -> String {
    let name = installed_mod.display_name();
    let local = installed_mod.version_label();
    let steam = installed_mod.steam_version.as_deref().unwrap_or("unknown");

    match language {
        UiLanguage::Korean => match installed_mod.update_status {
            ModUpdateStatus::Latest => {
                format!("최신: {}의 로컬 버전 {}와 Steam 버전 {}가 같습니다.", name, local, steam)
            }
            ModUpdateStatus::Outdated => {
                format!(
                    "업데이트 필요: {}의 로컬 버전은 {}, Steam 버전은 {}입니다.",
                    name, local, steam
                )
            }
            ModUpdateStatus::LocalNewer => {
                format!(
                    "로컬 버전 높음: {}의 로컬 버전은 {}, Steam 버전은 {}입니다. Steam 버전으로 맞추기 전에 확인이 필요합니다.",
                    name, local, steam
                )
            }
            ModUpdateStatus::OnlineAvailable => {
                let updated = installed_mod
                    .steam_updated_at
                    .map(|timestamp| format_timestamp(Some(timestamp)))
                    .unwrap_or_else(|| "unknown".to_string());
                format!(
                    "온라인 확인됨: {}는 Steam에 공개되어 있습니다. Steam 업데이트 시각은 {}이며, 정확한 버전 비교는 Steam 파일 다운로드 후 가능합니다.",
                    name, updated
                )
            }
            ModUpdateStatus::MissingSteamCache => match installed_mod.workshop_id {
                Some(workshop_id) => format!(
                    "확인 불가: Steam이 Workshop {}를 아직 다운로드하지 않았습니다. 업데이트를 누르면 Steam에서 받아 적용합니다.",
                    workshop_id
                ),
                None => format!("확인 불가: {}에 Workshop ID가 없습니다.", name),
            },
            ModUpdateStatus::Unknown => {
                format!("확인 불가: {}의 로컬/Steam 버전 정보를 비교할 수 없습니다.", name)
            }
            ModUpdateStatus::LocalOnly => {
                format!("로컬 전용: {}에 Workshop ID가 없습니다.", name)
            }
        },
        UiLanguage::English => match installed_mod.update_status {
            ModUpdateStatus::Latest => {
                format!("Latest: {} local {} matches Steam {}.", name, local, steam)
            }
            ModUpdateStatus::Outdated => {
                format!(
                    "Outdated: {} local version is {}, Steam version is {}.",
                    name, local, steam
                )
            }
            ModUpdateStatus::LocalNewer => {
                format!(
                    "Local newer: {} local version is {}, Steam version is {}. Confirm before matching Steam.",
                    name, local, steam
                )
            }
            ModUpdateStatus::OnlineAvailable => {
                let updated = installed_mod
                    .steam_updated_at
                    .map(|timestamp| format_timestamp(Some(timestamp)))
                    .unwrap_or_else(|| "unknown".to_string());
                format!(
                    "Online available: {} is visible on Steam. Steam updated at {}; exact version comparison requires downloading the Workshop files.",
                    name, updated
                )
            }
            ModUpdateStatus::MissingSteamCache => match installed_mod.workshop_id {
                Some(workshop_id) => format!(
                    "Unknown: Steam has not downloaded Workshop {} yet. Updating will download and apply it.",
                    workshop_id
                ),
                None => format!("Unknown: {} has no Workshop ID.", name),
            },
            ModUpdateStatus::Unknown => {
                format!("Unknown: {} local and Steam versions could not be compared.", name)
            }
            ModUpdateStatus::LocalOnly => {
                format!("Local only: {} has no Workshop ID.", name)
            }
        },
    }
}

fn parse_subscribe_notice_marker(log: &str) -> Option<u64> {
    let marker = "SUBSCRIBE_REQUIRED:";
    let index = log.find(marker)?;
    let id = log[index + marker.len()..]
        .trim_start()
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();

    id.parse::<u64>().ok().and_then(valid_workshop_id)
}

fn tr(language: UiLanguage, key: &'static str) -> &'static str {
    match language {
        UiLanguage::Korean => match key {
            "ready" => "준비됨",
            "game_folder" => "게임 폴더",
            "environment" => "환경 확인",
            "environment_check" => "환경 확인",
            "environment_not_checked" => "아직 환경을 확인하지 않았습니다.",
            "checking_environment" => "환경을 확인하는 중...",
            "environment_check_body" => "앱이 사용할 외부 구성 요소입니다. Steam과 게임은 앱에 포함할 수 없고, SteamCMD는 앱 데이터 폴더에 자동으로 준비할 수 있습니다.",
            "steam_client" => "Steam 클라이언트",
            "isaac_game" => "아이작 게임",
            "steam_libraries" => "Steam 라이브러리",
            "workshop_cache" => "Workshop 캐시",
            "steamcmd" => "SteamCMD",
            "steam_web_api" => "Steam Web API",
            "not_found" => "찾을 수 없음",
            "not_installed" => "아직 설치되지 않음",
            "reachable" => "연결 가능",
            "steam_library_paths" => "Steam 라이브러리 경로",
            "steamcmd_prepare_failed" => "SteamCMD 준비 실패",
            "environment_note" => "Steam 로그인 세션과 게임 본체는 Valve/Steam 쪽 구성이라 앱에 포함할 수 없습니다. 비공개 또는 친구 공개 Workshop 아이템은 Steam 앱에서 구독/다운로드된 캐시가 있어야 적용할 수 있습니다.",
            "prepare_steamcmd" => "SteamCMD 준비",
            "refresh" => "새로고침",
            "close" => "닫기",
            "error" => "오류",
            "available" => "사용 가능",
            "missing" => "없음",
            "auto_update" => "자동 업데이트",
            "exclude_auto_update" => "자동 업데이트 제외",
            "auto_excluded_short" => "자동 제외",
            "show_log" => "로그 표시",
            "language" => "언어",
            "path" => "경로",
            "not_selected" => "선택 안 됨",
            "status" => "상태",
            "progress" => "진행",
            "overall_progress" => "전체",
            "current_mod_progress" => "현재 모드",
            "installed_mods" => "설치된 모드:",
            "refresh_mods" => "새로고침",
            "search" => "검색",
            "search_hint" => "이름, 폴더, 버전, Workshop ID",
            "no_mods" => "모드 폴더가 없습니다.",
            "no_match" => "검색과 일치하는 모드가 없습니다.",
            "folder" => "폴더",
            "local_version" => "로컬 버전",
            "steam_version" => "Steam 버전",
            "version_status" => "버전 상태",
            "author" => "제작자",
            "workshop_id" => "Workshop ID",
            "local_only" => "로컬 전용",
            "description" => "설명",
            "no_workshop_id_meta" => "metadata.xml에 Workshop ID가 없습니다.",
            "retry_details" => "상세정보 다시 불러오기",
            "open_workshop_steam" => "Steam에서 Workshop 열기",
            "loading_details" => "Workshop 상세정보 불러오는 중...",
            "preview_unsupported" => "지원하지 않는 이미지 형식의 미리보기입니다.",
            "steam_updated" => "Steam 업데이트",
            "created" => "생성일",
            "size" => "크기",
            "stats" => "통계",
            "views" => "조회수",
            "subscriptions" => "구독",
            "favorites" => "즐겨찾기",
            "creator" => "제작자",
            "creators" => "제작자",
            "required_items" => "필수 아이템",
            "tags" => "태그",
            "opened_steam" => "Steam에서 Workshop 페이지를 열었습니다.",
            "opened_profile" => "Steam에서 제작자 프로필을 열었습니다.",
            "open_web_page" => "웹 페이지 열기",
            "download_apply" => "다운로드 & 적용",
            "update_all" => "모두 업데이트",
            "force_update" => "강제 업데이트",
            "force_update_title" => "강제 업데이트",
            "force_update_body" => "파일을 전부 다시 확인합니다. 최신으로 표시된 모드도 Workshop 파일과 비교한 뒤 필요한 파일을 다시 적용합니다.",
            "downloading_applying" => "Workshop 파일을 다운로드하고 적용하는 중...",
            "log" => "로그:",
            "select_mod" => "모드를 선택하세요.",
            "select_workshop_mod" => "Workshop 연결 모드를 먼저 선택하세요.",
            "no_updates" => "적용할 업데이트가 없습니다.",
            "updating_selected" => "선택한 모드를 업데이트하는 중...",
            "updating_all" => "모드를 업데이트하는 중:",
            "local_short" => "로컬",
            "confirm_downgrade_title" => "Steam 버전으로 맞추기 확인",
            "confirm_downgrade_single" => {
                "로컬 버전이 Steam 버전보다 높습니다. 개발 중인 변경사항이 덮어써질 수 있습니다. 정말 Steam 버전으로 맞출까요?"
            }
            "confirm_downgrade_all" => {
                "일부 모드의 로컬 버전이 Steam 버전보다 높습니다. 해당 모드들이 Steam 버전으로 덮어써질 수 있습니다. 계속할까요?"
            }
            "cancel" => "취소",
            "ok" => "확인",
            "match_steam_version" => "Steam 버전으로 맞추기",
            "subscribe_required_title" => "구독 필요",
            "subscribe_required_body" => {
                "Steam Workshop 파일 적용은 구독한 아이템만 가능합니다. Steam 창에서 구독한 뒤 다운로드가 끝나면 다시 적용하세요."
            }
            "mods_folder_missing" => "게임 폴더 안에 mods 폴더가 없습니다.",
            "no_installed_mods" => "설치된 모드를 찾지 못했습니다.",
            "no_workshop_linked_mods" => "mods 폴더에서 Workshop 연결 모드를 찾지 못했습니다.",
            "update_success" => "최신: 업데이트 적용이 완료되었습니다.",
            "already_up_to_date" => "최신: 이미 최신 버전입니다.",
            "update_failed" => "업데이트 실패.",
            "workshop_details_failed" => "Workshop 상세정보를 불러오지 못했습니다",
            "open_workshop_failed" => "Steam Workshop 페이지를 열지 못했습니다",
            "open_profile_failed" => "Steam 프로필을 열지 못했습니다",
            _ => key,
        },
        UiLanguage::English => match key {
            "ready" => "Ready",
            "game_folder" => "Game Folder",
            "environment" => "Environment",
            "environment_check" => "Environment Check",
            "environment_not_checked" => "The environment has not been checked yet.",
            "checking_environment" => "Checking environment...",
            "environment_check_body" => "These are the external pieces the app can use. Steam and the game cannot be bundled; SteamCMD can be prepared automatically in the app data folder.",
            "steam_client" => "Steam Client",
            "isaac_game" => "Isaac Game",
            "steam_libraries" => "Steam Libraries",
            "workshop_cache" => "Workshop Cache",
            "steamcmd" => "SteamCMD",
            "steam_web_api" => "Steam Web API",
            "not_found" => "Not found",
            "not_installed" => "Not installed yet",
            "reachable" => "Reachable",
            "steam_library_paths" => "Steam Library Paths",
            "steamcmd_prepare_failed" => "SteamCMD preparation failed",
            "environment_note" => "Steam login sessions and the game installation are controlled by Valve/Steam and cannot be bundled. Private or friends-only Workshop items still require a subscribed/downloaded Steam client cache before the app can apply them.",
            "prepare_steamcmd" => "Prepare SteamCMD",
            "refresh" => "Refresh",
            "close" => "Close",
            "error" => "Error",
            "available" => "Available",
            "missing" => "Missing",
            "auto_update" => "Auto update",
            "exclude_auto_update" => "Exclude from auto update",
            "auto_excluded_short" => "Auto excluded",
            "show_log" => "Show log",
            "language" => "Language",
            "path" => "Path",
            "not_selected" => "Not selected",
            "status" => "Status",
            "progress" => "Progress",
            "overall_progress" => "Overall",
            "current_mod_progress" => "Current mod",
            "installed_mods" => "Installed Mods:",
            "refresh_mods" => "Refresh Mods",
            "search" => "Search",
            "search_hint" => "name, folder, version, Workshop ID",
            "no_mods" => "No mod folders found.",
            "no_match" => "No mods match the current search.",
            "folder" => "Folder",
            "local_version" => "Local Version",
            "steam_version" => "Steam Version",
            "version_status" => "Version Status",
            "author" => "Author",
            "workshop_id" => "Workshop ID",
            "local_only" => "Local only",
            "description" => "Description",
            "no_workshop_id_meta" => "This mod has no Workshop ID in metadata.xml.",
            "retry_details" => "Retry Details",
            "open_workshop_steam" => "Open Workshop in Steam",
            "loading_details" => "Loading Workshop details...",
            "preview_unsupported" => "Preview is not a supported image format.",
            "steam_updated" => "Steam Updated",
            "created" => "Created",
            "size" => "Size",
            "stats" => "Stats",
            "views" => "Views",
            "subscriptions" => "Subscriptions",
            "favorites" => "Favorites",
            "creator" => "Creator",
            "creators" => "Creators",
            "required_items" => "Required Items",
            "tags" => "Tags",
            "opened_steam" => "Opened Workshop page in Steam.",
            "opened_profile" => "Opened creator profile in Steam.",
            "open_web_page" => "Open Web Page",
            "download_apply" => "Download & Apply",
            "update_all" => "Update All",
            "force_update" => "Force update",
            "force_update_title" => "Force Update",
            "force_update_body" => "All files will be checked again. Mods marked as latest will still be compared against Workshop files and reapplied where needed.",
            "downloading_applying" => "Downloading and applying workshop files...",
            "log" => "Log:",
            "select_mod" => "Select a mod.",
            "select_workshop_mod" => "Select a Workshop-linked mod first.",
            "no_updates" => "No updates to apply.",
            "updating_selected" => "Updating selected mod...",
            "updating_all" => "Updating mods:",
            "local_short" => "Local",
            "confirm_downgrade_title" => "Confirm Steam Version Match",
            "confirm_downgrade_single" => {
                "The local version is newer than Steam. Development changes may be overwritten. Match the Steam version?"
            }
            "confirm_downgrade_all" => {
                "Some local versions are newer than Steam. Those mods may be overwritten by Steam versions. Continue?"
            }
            "cancel" => "Cancel",
            "ok" => "OK",
            "match_steam_version" => "Match Steam Version",
            "subscribe_required_title" => "Subscription Required",
            "subscribe_required_body" => {
                "Only subscribed Steam Workshop items can be applied. Subscribe in Steam, wait for the download to finish, then apply again."
            }
            "mods_folder_missing" => "Mods folder not found inside game directory.",
            "no_installed_mods" => "No installed mods found.",
            "no_workshop_linked_mods" => "No Workshop-linked mod found in the mods folder.",
            "update_success" => "Latest: update applied successfully.",
            "already_up_to_date" => "Latest: already up to date.",
            "update_failed" => "Update failed.",
            "workshop_details_failed" => "Failed to load Workshop details",
            "open_workshop_failed" => "Could not open Steam Workshop page",
            "open_profile_failed" => "Could not open Steam profile",
            _ => key,
        },
    }
}

fn format_timestamp(timestamp: Option<u64>) -> String {
    let Some(timestamp) = timestamp else {
        return "unknown".to_string();
    };
    let time = UNIX_EPOCH + Duration::from_secs(timestamp);
    let datetime: DateTime<Local> = time.into();
    datetime.format("%Y-%m-%d %H:%M").to_string()
}

fn format_bytes(bytes: Option<u64>) -> String {
    let Some(bytes) = bytes else {
        return "unknown".to_string();
    };
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{:.1} {}", size, UNITS[unit])
    }
}

fn format_count(value: Option<u64>) -> String {
    value
        .map(format_number_with_commas)
        .unwrap_or_else(|| "?".to_string())
}

fn format_number_with_commas(value: u64) -> String {
    let digits = value.to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);

    for (index, ch) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            formatted.push(',');
        }
        formatted.push(ch);
    }

    formatted.chars().rev().collect()
}

fn workshop_url(app_id: u32, workshop_id: u64) -> String {
    format!(
        "https://steamcommunity.com/sharedfiles/filedetails/?id={}&searchtext=&appid={}",
        workshop_id, app_id
    )
}

fn workshop_public_url(workshop_id: u64) -> String {
    format!(
        "https://steamcommunity.com/sharedfiles/filedetails/?id={}",
        workshop_id
    )
}

fn steam_open_url(web_url: &str) -> String {
    format!("steam://openurl/{}", web_url)
}

fn open_workshop_in_steam(workshop_id: u64) -> anyhow::Result<()> {
    open_steam_or_web(&workshop_public_url(workshop_id))
}

fn open_steam_profile_url(profile_url: &str) -> anyhow::Result<()> {
    open_steam_or_web(profile_url)
}

fn open_steam_or_web(web_url: &str) -> anyhow::Result<()> {
    let steam_url = steam_open_url(web_url);

    #[cfg(target_os = "windows")]
    {
        if let Some(steam_dir) = crate::fs_utils::find_steam_path_from_registry() {
            let steam_exe = steam_dir.join("steam.exe");
            if steam_exe.exists() {
                Command::new(steam_exe).arg(&steam_url).spawn()?;
                return Ok(());
            }
        }

        Command::new("explorer").arg(web_url).spawn()?;
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        let opened_steam = Command::new("open")
            .arg(&steam_url)
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !opened_steam {
            Command::new("open").arg(web_url).spawn()?;
        }
        return Ok(());
    }

    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        let opened_steam = Command::new("xdg-open")
            .arg(&steam_url)
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !opened_steam {
            Command::new("xdg-open").arg(web_url).spawn()?;
        }
        return Ok(());
    }
}

fn scan_installed_mods(
    mods_path: &Path,
    app_id: u32,
    steam_roots: &[PathBuf],
) -> Vec<InstalledMod> {
    let Ok(entries) = fs::read_dir(mods_path) else {
        return Vec::new();
    };

    let mut mods = Vec::new();
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }

        let path = entry.path();
        let folder_name = entry.file_name().to_string_lossy().to_string();
        let metadata = read_local_metadata(&path).unwrap_or_default();
        let workshop_id = workshop_id_from_metadata(&folder_name, &metadata);
        let (steam_version, update_status) = determine_update_status(
            app_id,
            workshop_id,
            metadata.version.as_deref(),
            steam_roots,
        );

        mods.push(InstalledMod {
            path,
            folder_name,
            name: metadata.name,
            version: metadata.version,
            description: metadata.description,
            author: metadata.author,
            workshop_id,
            steam_version,
            steam_title: None,
            steam_updated_at: None,
            update_status,
        });
    }

    enrich_missing_cache_mods_from_steam(&mut mods);

    mods.sort_by(|left, right| {
        update_status_priority(&left.update_status)
            .cmp(&update_status_priority(&right.update_status))
            .then_with(|| left.display_name().cmp(right.display_name()))
    });

    mods
}

fn enrich_missing_cache_mods_from_steam(mods: &mut [InstalledMod]) {
    let ids = mods
        .iter()
        .filter(|installed_mod| installed_mod.update_status == ModUpdateStatus::MissingSteamCache)
        .filter_map(|installed_mod| installed_mod.workshop_id)
        .filter_map(valid_workshop_id)
        .collect::<Vec<_>>();

    if ids.is_empty() {
        return;
    }

    let Ok(summaries) = fetch_workshop_summaries(&ids) else {
        return;
    };

    for installed_mod in mods {
        let Some(workshop_id) = installed_mod.workshop_id.and_then(valid_workshop_id) else {
            continue;
        };
        let Some(summary) = summaries.get(&workshop_id) else {
            continue;
        };

        installed_mod.steam_title = Some(summary.title.clone());
        installed_mod.steam_updated_at = summary.time_updated;
        if installed_mod.update_status == ModUpdateStatus::MissingSteamCache {
            installed_mod.update_status = ModUpdateStatus::OnlineAvailable;
        }
    }
}

fn read_local_metadata(mod_path: &Path) -> Option<LocalMetadata> {
    let metadata_path = mod_path.join("metadata.xml");
    let content = read_text_file(&metadata_path).ok()?;
    quick_xml::de::from_str(&content).ok()
}

fn determine_update_status(
    app_id: u32,
    workshop_id: Option<u64>,
    local_version: Option<&str>,
    steam_roots: &[PathBuf],
) -> (Option<String>, ModUpdateStatus) {
    let Some(workshop_id) = workshop_id else {
        return (None, ModUpdateStatus::LocalOnly);
    };

    let Some(cache_path) = find_cached_workshop_item(app_id, workshop_id, steam_roots) else {
        return (None, ModUpdateStatus::MissingSteamCache);
    };

    let Some(cache_metadata) = read_local_metadata(&cache_path) else {
        return (None, ModUpdateStatus::Unknown);
    };

    let local = normalize_version(local_version);
    let steam = normalize_version(cache_metadata.version.as_deref());
    let status = match (local.as_deref(), steam.as_deref()) {
        (Some(local), Some(steam)) if local == steam => ModUpdateStatus::Latest,
        (Some(local), Some(steam)) => match compare_version_strings(local, steam) {
            Some(Ordering::Less) => ModUpdateStatus::Outdated,
            Some(Ordering::Greater) => ModUpdateStatus::LocalNewer,
            Some(Ordering::Equal) => ModUpdateStatus::Latest,
            None => ModUpdateStatus::Unknown,
        },
        (None, Some(_)) => ModUpdateStatus::Outdated,
        (Some(_), None) | (None, None) => ModUpdateStatus::Unknown,
    };

    (steam, status)
}

fn update_status_priority(status: &ModUpdateStatus) -> u8 {
    match status {
        ModUpdateStatus::Outdated => 0,
        ModUpdateStatus::LocalNewer => 1,
        ModUpdateStatus::OnlineAvailable => 2,
        ModUpdateStatus::MissingSteamCache => 3,
        ModUpdateStatus::Unknown => 4,
        ModUpdateStatus::Latest => 5,
        ModUpdateStatus::LocalOnly => 6,
    }
}

fn normalize_version(version: Option<&str>) -> Option<String> {
    version
        .map(str::trim)
        .filter(|version| !version.is_empty())
        .map(ToOwned::to_owned)
}

fn valid_workshop_id(workshop_id: u64) -> Option<u64> {
    (workshop_id > 0).then_some(workshop_id)
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

fn workshop_id_from_metadata(folder_name: &str, metadata: &LocalMetadata) -> Option<u64> {
    if let Some(workshop_id) = metadata
        .id
        .as_deref()
        .and_then(|id| id.trim().parse::<u64>().ok())
        .and_then(valid_workshop_id)
    {
        return Some(workshop_id);
    }

    if metadata.directory.as_deref() == Some(SUPPORTED_MOD_DIRECTORY) {
        return Some(CONCH_BLESSING_WORKSHOP_ID);
    }

    if folder_name == SUPPORTED_MOD_DIRECTORY || folder_name.starts_with("conch_blessing_") {
        return Some(CONCH_BLESSING_WORKSHOP_ID);
    }

    if metadata.name.as_deref().is_some_and(|name| {
        let lower = name.to_ascii_lowercase();
        lower.contains("conch") && lower.contains("blessing")
    }) {
        return Some(CONCH_BLESSING_WORKSHOP_ID);
    }

    None
}

#[cfg(target_os = "windows")]
fn save_config(path: &Path) -> anyhow::Result<()> {
    use winreg::enums::*;
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu.create_subkey(SETTINGS_REGISTRY_KEY)?;
    key.set_value("IsaacPath", &path.to_string_lossy().as_ref())?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn load_config() -> Option<PathBuf> {
    use winreg::enums::*;
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu
        .open_subkey(SETTINGS_REGISTRY_KEY)
        .or_else(|_| hkcu.open_subkey(LEGACY_SETTINGS_REGISTRY_KEY))
        .ok()?;
    let path_str: String = key.get_value("IsaacPath").ok()?;
    Some(PathBuf::from(path_str))
}

#[cfg(target_os = "windows")]
fn save_auto_update(enabled: bool) -> anyhow::Result<()> {
    use winreg::enums::*;
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu.create_subkey(SETTINGS_REGISTRY_KEY)?;
    let value: u32 = if enabled { 1 } else { 0 };
    key.set_value("AutoUpdate", &value)?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn load_auto_update() -> Option<bool> {
    use winreg::enums::*;
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu
        .open_subkey(SETTINGS_REGISTRY_KEY)
        .or_else(|_| hkcu.open_subkey(LEGACY_SETTINGS_REGISTRY_KEY))
        .ok()?;
    let value: u32 = key.get_value("AutoUpdate").ok()?;
    Some(value != 0)
}

#[cfg(target_os = "windows")]
fn save_auto_update_exclusions(exclusions: &HashSet<u64>) -> anyhow::Result<()> {
    use winreg::enums::*;
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu.create_subkey(SETTINGS_REGISTRY_KEY)?;
    let mut ids = exclusions.iter().copied().collect::<Vec<_>>();
    ids.sort_unstable();
    let value = ids
        .into_iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(";");
    key.set_value("AutoUpdateExclusions", &value)?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn load_auto_update_exclusions() -> Option<HashSet<u64>> {
    use winreg::enums::*;
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu
        .open_subkey(SETTINGS_REGISTRY_KEY)
        .or_else(|_| hkcu.open_subkey(LEGACY_SETTINGS_REGISTRY_KEY))
        .ok()?;
    let value: String = key.get_value("AutoUpdateExclusions").ok()?;
    Some(parse_workshop_id_set(&value))
}

#[cfg(target_os = "windows")]
fn save_language_mode(mode: LanguageMode) -> anyhow::Result<()> {
    use winreg::enums::*;
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu.create_subkey(SETTINGS_REGISTRY_KEY)?;
    key.set_value("LanguageMode", &mode.as_str())?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn load_language_mode() -> Option<LanguageMode> {
    use winreg::enums::*;
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu
        .open_subkey(SETTINGS_REGISTRY_KEY)
        .or_else(|_| hkcu.open_subkey(LEGACY_SETTINGS_REGISTRY_KEY))
        .ok()?;
    let value: String = key.get_value("LanguageMode").ok()?;
    LanguageMode::from_str(&value)
}

#[cfg(not(target_os = "windows"))]
fn save_config(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn load_config() -> Option<PathBuf> {
    None
}

#[cfg(not(target_os = "windows"))]
fn save_auto_update(_enabled: bool) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn load_auto_update() -> Option<bool> {
    None
}

#[cfg(not(target_os = "windows"))]
fn save_auto_update_exclusions(_exclusions: &HashSet<u64>) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn load_auto_update_exclusions() -> Option<HashSet<u64>> {
    None
}

#[cfg(not(target_os = "windows"))]
fn save_language_mode(_mode: LanguageMode) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn load_language_mode() -> Option<LanguageMode> {
    None
}

fn parse_workshop_id_set(value: &str) -> HashSet<u64> {
    value
        .split([';', ',', ' ', '\n', '\r', '\t'])
        .filter_map(|id| id.trim().parse::<u64>().ok())
        .filter_map(valid_workshop_id)
        .collect()
}
