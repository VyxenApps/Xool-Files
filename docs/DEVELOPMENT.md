# Xool Files development guide

Created by **Vyxen Apps**. [VyxenApps on GitHub](https://github.com/VyxenApps)

## Requirements

- 64-bit Windows and PowerShell.
- Rust with support for the 2024 edition and the `x86_64-pc-windows-msvc` toolchain.
- Visual Studio C++ Build Tools and the Windows SDK. The build uses `link.exe`, `rc.exe`, and `fxc.exe`.
- Access to the Rust package registry on the first build, or a cache containing the dependencies pinned in `Cargo.lock`.

## Build and run

| Command | Result |
| --- | --- |
| `.\build.ps1` | Builds in release mode and copies `Xool Files.exe` to the project root. |
| `.\build.ps1 -NoCopy` | Builds `target\release\xool-files.exe`. |
| `.\build.ps1 -Check` | Checks the code without building a release executable. |
| `.\build.ps1 -Test` | Runs the automated tests. |
| `.\build.ps1 -Dev -Run` | Builds and runs Xool Files in development mode. |

Set the following environment variable before running the app to keep the test profile separate.

```powershell
$env:XOOLFILES_DATA_DIR = Join-Path $env:TEMP 'XoolFiles-dev'
.\build.ps1 -Dev -Run
```

The build script accepts an optional `build.local.json` next to `build.ps1`. Its `cargo` and `cargo_home` fields specify local paths. Git ignores this file.

## Project layout

| Path | Contents |
| --- | --- |
| `src/main.rs`, `src/app.rs`, and `src/ui.rs` | Startup, application state, and user interface. |
| `src/fs_model.rs` and `src/search_index.rs` | File views, filters, the Recycle Bin, and the search index. |
| `src/settings.rs` and `src/shortcuts.rs` | Preferences, settings compatibility, and shortcuts. |
| `src/platform.rs`, `src/integration.rs`, and `src/shell_menu.rs` | Windows integration and system actions. |
| `assets/` | Icons and other bundled resources. |
| `vendor/` | Patched dependencies and their license notices. |

## Publish a release on GitHub

1. Publish the project root with `Cargo.lock`, `assets/`, `vendor/`, and the license notices.
2. Build and run the tests. On Windows, verify navigation, split panes, search, the Recycle Bin, and startup with an isolated profile.
3. Attach the built executable to the release and offer the corresponding source for that exact build at no extra charge. Keep `target/`, local executables, personal settings, and logs out of the source repository.
