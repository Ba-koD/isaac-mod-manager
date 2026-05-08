# Isaac Mod Manager

Steam Workshop updater for The Binding of Isaac: Rebirth.

The app scans installed local mods, reads each mod's Workshop ID from `metadata.xml`, uses Steam's local Workshop cache when available, then syncs the selected Workshop files into the selected local mod folder.

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
target\release\isaac_mod_manager.exe
```

If SteamCMD is not available on `PATH`, the app downloads Valve's SteamCMD into:

```text
%LOCALAPPDATA%\Ba-koD\isaac_mod_manager\steamcmd
```
