# Isaac Mod Manager

Steam Workshop updater for The Binding of Isaac: Rebirth.

The app scans installed local mods, reads each mod's Workshop ID from `metadata.xml`, uses Steam's local Workshop cache when available, then syncs the selected Workshop files into the selected local mod folder.

Mods with a local `disable.it` file are shown as disabled and can be toggled from the manager. Updates and force updates preserve this file so a disabled mod stays disabled after syncing Workshop files.

The app version is managed with Git tags. On startup, the app checks the latest GitHub Release and can install a newer Windows release asset in place.

The UI also supports mod search and a details panel backed by Steam's public Workshop details API. When the Steam client has not downloaded the item yet, the app tries SteamCMD anonymous fallback. Manual updates show download/apply output in the in-app log.

The app embeds `NotoSansCJKkr-Regular.otf` from Noto Sans CJK for Korean/Japanese/Chinese fallback text rendering. The font is distributed under the SIL Open Font License; see `third_party\noto-cjk\LICENSE`.

## Local Test

Run the app:

```powershell
cargo run
```

Check, test, or build:

```powershell
cargo check-local
cargo test-local
cargo build-local
```

Or run the full local test/build script:

```powershell
.\scripts\local-test.ps1
```

To test and launch the app after building:

```powershell
.\scripts\local-test.ps1 -Run
```

Release binary:

```text
target\release\isaac-mod-manager.exe
```

Create a versioned GitHub Release:

```powershell
git tag v1.0.0
git push origin v1.0.0
```

If SteamCMD is not available on `PATH`, the app downloads Valve's SteamCMD into:

```text
%LOCALAPPDATA%\Ba-koD\isaac_mod_manager\steamcmd
```
