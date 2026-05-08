use directories::UserDirs;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

#[cfg(target_os = "windows")]
pub fn find_steam_path_from_registry() -> Option<PathBuf> {
    use winreg::enums::*;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let steam = hkcu.open_subkey("Software\\Valve\\Steam").ok()?;
    let path_str: String = steam.get_value("SteamPath").ok()?;

    Some(PathBuf::from(path_str))
}

pub fn find_steam_from_path_env() -> Option<PathBuf> {
    if let Some(paths) = std::env::var_os("PATH") {
        for path in std::env::split_paths(&paths) {
            // Check for steam.exe (Windows) or steam (Unix)
            let steam_exe = path.join("steam.exe");
            if steam_exe.exists() {
                return Some(path);
            }
        }
    }
    None
}

pub fn find_isaac_game_path() -> Option<PathBuf> {
    // 1. Try Windows Registry (Windows only)
    #[cfg(target_os = "windows")]
    {
        if let Some(steam_path) = find_steam_path_from_registry() {
            let game_path = steam_path.join("steamapps/common/The Binding of Isaac Rebirth");
            if game_path.join("isaac-ng.exe").exists() {
                return Some(game_path);
            }
        }
    }

    // 2. Try PATH environment variable
    if let Some(steam_path) = find_steam_from_path_env() {
        let game_path = steam_path.join("steamapps/common/The Binding of Isaac Rebirth");
        if game_path.exists() {
            // Weak check if exe not visible in PATH lookup context
            return Some(game_path);
        }
    }

    // 3. Fallback to common Steam paths
    let common_steam_paths = [
        r"C:\Program Files (x86)\Steam",
        r"C:\Steam",
        r"D:\Steam",
        r"E:\Steam",
        // Common library paths
        r"C:\SteamLibrary",
        r"D:\SteamLibrary",
        r"E:\SteamLibrary",
    ];

    for p in common_steam_paths {
        let base_path = if p.starts_with("~") {
            if let Some(user_dirs) = UserDirs::new() {
                let home = user_dirs.home_dir();
                let suffix = &p[2..];
                home.join(suffix)
            } else {
                PathBuf::from(p)
            }
        } else {
            PathBuf::from(p)
        };

        if base_path.exists() {
            let game_path = base_path.join("steamapps/common/The Binding of Isaac Rebirth");
            // Check for game executable
            let exe_name = if cfg!(target_os = "windows") {
                "isaac-ng.exe"
            } else {
                "isaac-ng"
            };
            // Note: Mac might be different (Isaac-ng), Linux (isaac-ng).

            if game_path.join(exe_name).exists() || game_path.exists() {
                return Some(game_path);
            }
        }
    }

    // 3. Check specific Mac save data path (standard location for mods on Mac, but game is elsewhere)
    // Skipping Mac specific game path detection for now as user emphasized Windows.

    None
}

pub fn find_steam_library_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    #[cfg(target_os = "windows")]
    if let Some(steam_path) = find_steam_path_from_registry() {
        roots.push(steam_path.clone());
        roots.extend(read_libraryfolders_vdf(&steam_path));
    }

    for path in common_steam_roots() {
        if path.exists() {
            roots.push(path.clone());
            roots.extend(read_libraryfolders_vdf(&path));
        }
    }

    dedup_existing_paths(roots)
}

fn common_steam_roots() -> Vec<PathBuf> {
    vec![
        PathBuf::from(r"C:\Program Files (x86)\Steam"),
        PathBuf::from(r"C:\Steam"),
        PathBuf::from(r"D:\Steam"),
        PathBuf::from(r"E:\Steam"),
        PathBuf::from(r"C:\SteamLibrary"),
        PathBuf::from(r"D:\SteamLibrary"),
        PathBuf::from(r"E:\SteamLibrary"),
    ]
}

fn read_libraryfolders_vdf(steam_root: &PathBuf) -> Vec<PathBuf> {
    let path = steam_root.join("steamapps").join("libraryfolders.vdf");
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };

    content
        .lines()
        .filter_map(parse_library_path_line)
        .map(PathBuf::from)
        .collect()
}

fn parse_library_path_line(line: &str) -> Option<String> {
    let line = line.trim();
    if !line.starts_with("\"path\"") {
        return None;
    }

    let mut quoted = line.split('"').filter(|part| !part.is_empty());
    let key = quoted.next()?;
    if key != "path" {
        return None;
    }
    let value = quoted.next()?;
    Some(value.replace("\\\\", "\\"))
}

fn dedup_existing_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut output = Vec::new();

    for path in paths {
        if !path.exists() {
            continue;
        }

        let key = path.to_string_lossy().to_ascii_lowercase();
        if seen.insert(key) {
            output.push(path);
        }
    }

    output
}
