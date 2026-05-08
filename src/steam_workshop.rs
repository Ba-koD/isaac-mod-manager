use anyhow::{Context, Result};
use encoding_rs::EUC_KR;
use reqwest::blocking::Client;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use zip::ZipArchive;

pub const ISAAC_APP_ID: u32 = 250900;
pub const CONCH_BLESSING_WORKSHOP_ID: u64 = 3545334858;

const STEAMCMD_ZIP_URL: &str = "https://steamcdn-a.akamaihd.net/client/installer/steamcmd.zip";
const DEFAULT_STEAM_CLIENT_DOWNLOAD_WAIT: Duration = Duration::from_secs(20);
const STEAM_CLIENT_DOWNLOAD_POLL: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub struct SteamWorkshopClient {
    app_id: u32,
    workshop_id: u64,
    steam_library_roots: Vec<PathBuf>,
    steam_client_download_wait: Duration,
    steamcmd_lock: Option<Arc<Mutex<()>>>,
    force_download: bool,
}

impl SteamWorkshopClient {
    pub fn new(app_id: u32, workshop_id: u64) -> Self {
        Self {
            app_id,
            workshop_id,
            steam_library_roots: Vec::new(),
            steam_client_download_wait: DEFAULT_STEAM_CLIENT_DOWNLOAD_WAIT,
            steamcmd_lock: None,
            force_download: false,
        }
    }

    pub fn with_steam_library_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.steam_library_roots = roots;
        self
    }

    pub fn with_steam_client_download_wait(mut self, wait: Duration) -> Self {
        self.steam_client_download_wait = wait;
        self
    }

    pub fn with_steamcmd_lock(mut self, lock: Arc<Mutex<()>>) -> Self {
        self.steamcmd_lock = Some(lock);
        self
    }

    pub fn with_force_download(mut self, force_download: bool) -> Self {
        self.force_download = force_download;
        self
    }

    pub fn download_latest(&self, logger: Option<&dyn Fn(String)>) -> Result<PathBuf> {
        if let Some(path) =
            find_cached_workshop_item(self.app_id, self.workshop_id, &self.steam_library_roots)
        {
            let action = if self.force_download {
                "Force update enabled; using Steam client workshop cache and verifying all files"
            } else {
                "Using Steam client workshop cache"
            };
            log(logger, format!("{}: {}", action, path.to_string_lossy()));
            return Ok(path);
        }

        log(
            logger,
            "Trying SteamCMD anonymous workshop download...".to_string(),
        );
        let anonymous_failed = {
            let _steamcmd_guard = self
                .steamcmd_lock
                .as_ref()
                .map(|lock| {
                    log(logger, "Waiting for SteamCMD slot...".to_string());
                    lock.lock()
                })
                .transpose()
                .map_err(|_| anyhow::anyhow!("SteamCMD lock was poisoned"))?;

            let steamcmd = ensure_steamcmd(logger)?;
            let steamcmd_dir = steamcmd
                .parent()
                .context("SteamCMD path has no parent directory")?;

            let app_id = self.app_id.to_string();
            let workshop_id = self.workshop_id.to_string();
            let args = self.steamcmd_args(&app_id, &workshop_id)?;

            let output = run_steamcmd_streaming(&steamcmd, steamcmd_dir, args, logger)?;
            let combined_lower = output.to_ascii_lowercase();

            if combined_lower.contains("error!") || combined_lower.contains("download item failed")
            {
                true
            } else {
                let content_dir = steamcmd_dir
                    .join("steamapps")
                    .join("workshop")
                    .join("content")
                    .join(app_id)
                    .join(workshop_id);

                if content_dir.exists() {
                    log(
                        logger,
                        format!("Steam workshop content ready: {}", content_dir.display()),
                    );
                    return Ok(content_dir);
                }

                return Err(anyhow::anyhow!(
                    "SteamCMD finished but workshop content was not found at {}",
                    content_dir.display()
                ));
            }
        };

        if !anonymous_failed {
            unreachable!("SteamCMD success path returns before reaching client fallback");
        }

        if let Some(path) =
            find_cached_workshop_item(self.app_id, self.workshop_id, &self.steam_library_roots)
        {
            log(
                logger,
                format!(
                    "SteamCMD failed, but Steam client workshop cache is available: {}",
                    path.display()
                ),
            );
            return Ok(path);
        }

        log(
            logger,
            "SteamCMD anonymous download failed. Opening the Workshop page in the logged-in Steam client...".to_string(),
        );
        open_workshop_page(self.workshop_id, logger)?;
        log(
            logger,
            "Waiting for Steam client workshop cache. If the item is already subscribed, wait for Steam downloads to finish.".to_string(),
        );
        if let Some(path) = wait_for_steam_client_cache(
            self.app_id,
            self.workshop_id,
            &self.steam_library_roots,
            self.steam_client_download_wait,
            logger,
        ) {
            return Ok(path);
        }

        log(logger, format!("SUBSCRIBE_REQUIRED:{}", self.workshop_id));
        Err(anyhow::anyhow!(
            "Steam client workshop cache was not found yet. Make sure the logged-in Steam account can access this item, subscribe/download it in Steam, wait for downloads to finish, then retry."
        ))
    }

    fn steamcmd_args(&self, app_id: &str, workshop_id: &str) -> Result<Vec<String>> {
        let mut args = Vec::new();

        args.push("+login".to_string());
        args.push("anonymous".to_string());
        args.push("+workshop_download_item".to_string());
        args.push(app_id.to_string());
        args.push(workshop_id.to_string());
        args.push("validate".to_string());
        args.push("+quit".to_string());

        Ok(args)
    }
}

fn wait_for_steam_client_cache(
    app_id: u32,
    workshop_id: u64,
    steam_library_roots: &[PathBuf],
    wait: Duration,
    logger: Option<&dyn Fn(String)>,
) -> Option<PathBuf> {
    if wait.is_zero() {
        return find_cached_workshop_item(app_id, workshop_id, steam_library_roots);
    }

    let started = Instant::now();
    let mut next_log = Duration::ZERO;

    loop {
        if let Some(path) = find_cached_workshop_item(app_id, workshop_id, steam_library_roots) {
            log(
                logger,
                format!("Steam client workshop cache is ready: {}", path.display()),
            );
            return Some(path);
        }

        let elapsed = started.elapsed();
        if elapsed >= wait {
            return None;
        }

        if elapsed >= next_log {
            log(
                logger,
                format!(
                    "Waiting briefly for Steam client download... {}s remaining",
                    wait.saturating_sub(elapsed).as_secs()
                ),
            );
            next_log += Duration::from_secs(15);
        }

        thread::sleep(STEAM_CLIENT_DOWNLOAD_POLL);
    }
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

fn open_workshop_page(workshop_id: u64, logger: Option<&dyn Fn(String)>) -> Result<()> {
    let web_url = workshop_public_url(workshop_id);
    let steam_url = steam_open_url(&web_url);

    #[cfg(target_os = "windows")]
    {
        if let Some(steam_dir) = crate::fs_utils::find_steam_path_from_registry() {
            let steam_exe = steam_dir.join("steam.exe");
            if steam_exe.exists() {
                log(logger, format!("Opening Workshop in Steam: {}", web_url));
                Command::new(steam_exe).arg(&steam_url).spawn()?;
                return Ok(());
            }
        }

        log(logger, format!("Opening Workshop in browser: {}", web_url));
        Command::new("explorer").arg(web_url).spawn()?;
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        log(logger, format!("Opening Workshop in Steam: {}", web_url));
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
        log(logger, format!("Opening Workshop in Steam: {}", web_url));
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

pub fn find_cached_workshop_item(
    app_id: u32,
    workshop_id: u64,
    steam_library_roots: &[PathBuf],
) -> Option<PathBuf> {
    let app_id = app_id.to_string();
    let workshop_id = workshop_id.to_string();

    for root in steam_library_roots {
        let candidates = [
            root.join("steamapps")
                .join("workshop")
                .join("content")
                .join(&app_id)
                .join(&workshop_id),
            root.join("workshop")
                .join("content")
                .join(&app_id)
                .join(&workshop_id),
        ];

        for candidate in candidates {
            if is_usable_workshop_dir(&candidate) {
                return Some(candidate);
            }
        }
    }

    None
}

fn run_steamcmd_streaming(
    steamcmd: &Path,
    steamcmd_dir: &Path,
    args: Vec<String>,
    logger: Option<&dyn Fn(String)>,
) -> Result<String> {
    let mut command = Command::new(steamcmd);
    command
        .current_dir(steamcmd_dir)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = command.spawn().context("Failed to run steamcmd")?;

    let (tx, rx) = mpsc::channel();
    let mut readers = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        readers.push(spawn_output_reader(stdout, tx.clone()));
    }
    if let Some(stderr) = child.stderr.take() {
        readers.push(spawn_output_reader(stderr, tx.clone()));
    }
    drop(tx);

    let mut combined = String::new();
    let status = wait_for_process_with_output(&mut child, &rx, logger, &mut combined)?;

    for reader in readers {
        let _ = reader.join();
    }
    for line in rx.try_iter() {
        append_output_line(logger, &mut combined, line);
    }

    if !status.success() {
        return Err(anyhow::anyhow!(
            "SteamCMD exited with status {}",
            status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "terminated".to_string())
        ));
    }

    Ok(combined)
}

fn is_usable_workshop_dir(path: &Path) -> bool {
    path.is_dir()
        && fs::read_dir(path)
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(false)
}

fn wait_for_process_with_output(
    child: &mut std::process::Child,
    rx: &mpsc::Receiver<String>,
    logger: Option<&dyn Fn(String)>,
    combined: &mut String,
) -> Result<ExitStatus> {
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => append_output_line(logger, combined, line),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {}
        }

        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
    }
}

fn spawn_output_reader<R: Read + Send + 'static>(
    reader: R,
    tx: mpsc::Sender<String>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut buffer = Vec::new();
        loop {
            buffer.clear();
            let Ok(count) = reader.read_until(b'\n', &mut buffer) else {
                break;
            };
            if count == 0 {
                break;
            }

            let line = decode_process_output(&buffer);
            let line = line.trim_end_matches(['\r', '\n']).to_string();
            if !line.trim().is_empty() && tx.send(line).is_err() {
                break;
            }
        }
    })
}

fn append_output_line(logger: Option<&dyn Fn(String)>, combined: &mut String, line: String) {
    log(logger, line.clone());
    combined.push_str(&line);
    combined.push('\n');
}

pub fn find_steamcmd() -> Option<PathBuf> {
    if let Some(path) = env::var_os("STEAMCMD_PATH").map(PathBuf::from) {
        if path.exists() {
            return Some(path);
        }
    }

    if let Some(path) = find_steamcmd_in_path() {
        return Some(path);
    }

    let Ok(steamcmd) = local_steamcmd_path() else {
        return None;
    };
    steamcmd.exists().then_some(steamcmd)
}

pub fn prepare_steamcmd(logger: Option<&dyn Fn(String)>) -> Result<PathBuf> {
    ensure_steamcmd(logger)
}

fn ensure_steamcmd(logger: Option<&dyn Fn(String)>) -> Result<PathBuf> {
    if let Some(path) = find_steamcmd() {
        return Ok(path);
    }

    let steamcmd = local_steamcmd_path()?;
    let install_dir = steamcmd
        .parent()
        .context("SteamCMD install path has no parent directory")?;
    fs::create_dir_all(&install_dir)?;
    log(
        logger,
        format!("Downloading SteamCMD to {}...", install_dir.display()),
    );

    let bytes = Client::builder()
        .user_agent("isaac_mod_manager")
        .build()?
        .get(STEAMCMD_ZIP_URL)
        .send()?
        .error_for_status()?
        .bytes()?;

    let cursor = Cursor::new(bytes);
    let mut archive = ZipArchive::new(cursor)?;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let Some(file_name) = Path::new(file.name()).file_name() else {
            continue;
        };
        if file_name == "steamcmd.exe" {
            let output_path = install_dir.join(file_name);
            let mut output = fs::File::create(&output_path)?;
            std::io::copy(&mut file, &mut output)?;
            return Ok(output_path);
        }
    }

    Err(anyhow::anyhow!(
        "steamcmd.exe was not found in downloaded SteamCMD archive"
    ))
}

fn find_steamcmd_in_path() -> Option<PathBuf> {
    let paths = env::var_os("PATH")?;
    for path in env::split_paths(&paths) {
        let candidate = path.join("steamcmd.exe");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn local_steamcmd_path() -> Result<PathBuf> {
    Ok(local_app_dir()?.join("steamcmd").join("steamcmd.exe"))
}

fn local_app_dir() -> Result<PathBuf> {
    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        return Ok(PathBuf::from(local_app_data)
            .join("Ba-koD")
            .join("isaac_mod_manager"));
    }

    Ok(env::current_dir()?.join(".isaac_mod_manager"))
}

fn decode_process_output(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }

    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => {
            let (decoded, _, _) = EUC_KR.decode(bytes);
            decoded.into_owned()
        }
    }
}

fn log(logger: Option<&dyn Fn(String)>, msg: String) {
    if let Some(f) = logger {
        f(msg.clone());
    }
    println!("{}", msg);
}
