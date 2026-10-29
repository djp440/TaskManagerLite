# TaskManagerLite

[简体中文](README.md) | **English**

A Windows Task Manager rewritten in Rust with [Slint](https://slint.dev): fast cold start, low idle overhead, and a polished UI.

![Processes (dark)](docs/images/processes-dark.png)

| Performance | Services |
| --- | --- |
| ![Performance: CPU (dark)](docs/images/performance-cpu-dark.png) | ![Services page (dark)](docs/images/services-dark.png) |

## Features

- **Processes**: sorting, search and process icons; flat, process-tree, and "Apps / Background processes" grouped views; optional "Details" columns (column picker, horizontal scrolling); end a process or a whole process tree.
- **Context menu**: open file location, copy path and command line, properties, search online.
- **Performance**: CPU (with a logical-processor view, multiple processor groups supported), memory (composition, commit, slots and speed), per-physical-disk (with partition info), per-network-adapter (model, virtual adapters), and GPU (model, VRAM graph). Optional ETW-based per-process disk rates for better accuracy (requires administrator rights, off by default).
- **Startup apps**: enumerates Run / Run32 (HKCU, HKLM) and the startup folders; enabled/disabled state is shared with the built-in Task Manager; machine-wide entries are changed through a UAC child process.
- **Services**: name, display name, description, status, start type, PID and group; start, stop and restart (administrator rights required, failures show the reason); jump to the matching PID on the Processes page.
- **Mini mode**: an always-on-top small window showing only the resource cards.
- **Settings**: start with Windows, dark / light / follow-system theme, minimize to tray on close, keep sampling in the background, always on top, and taking over Ctrl+Shift+Esc (off by default).
- **Elevate on demand**: starts without administrator rights and never shows UAC up front; restarts as administrator only when you need to inspect or end protected processes.
- **Light footprint**: adjustable sampling interval (0.5 / 1 / 2 s or paused), sampling stops while the window is hidden; single instance only.
- **Persistent settings**: window position and size, column widths and more are stored in `%APPDATA%\TaskManagerLite\settings.json`.

<details>
<summary>More screenshots</summary>

| Startup apps | Mini mode | Settings |
| --- | --- | --- |
| ![Startup apps (dark)](docs/images/startup-dark.png) | ![Mini mode (dark)](docs/images/mini-dark.png) | ![Settings (dark)](docs/images/settings-dark.png) |

| GPU | Light theme |
| --- | --- |
| ![GPU (dark)](docs/images/performance-gpu-dark.png) | ![Processes (light)](docs/images/processes-light.png) |

</details>

Measured startup time and idle overhead are in [ADR 0002](docs/adr/0002-software-renderer-default.md) (Chinese).

## Download

Get `TaskManagerLite-<version>.exe` from [GitHub Releases](https://github.com/djp440/TaskManagerLite/releases); the `.sha256` file next to it can be used to verify the download. No installation needed, just run it. The UI is in Chinese.

## Build from source

Requires Windows and the stable Rust toolchain.

```sh
# Default: software renderer
cargo build -p tml-app --release

# femtovg (OpenGL) renderer
cargo build -p tml-app --release --no-default-features --features renderer-femtovg

# Tests
cargo test -p tml-core -p tml-app
```

The output is `target/release/TaskManagerLite.exe`.

## Project layout

- `crates/core` (`tml-core`): collects process snapshots and system metrics, builds the process tree, ends processes, enumerates startup entries and services. Pure Rust, unit-testable without the UI.
- `crates/app` (`tml-app`): the Slint UI and the sampling thread.
- `scripts/`: startup-time and idle-overhead measurement, screenshot, and end-to-end acceptance scripts.
- `docs/adr/`: architecture decision records; `docs/checklist/`: manual acceptance checklists.
- `GLOSSARY.md`: domain glossary (Chinese).

## License

Released under the [MIT License](LICENSE).

The UI is built with Slint under the [Slint Royalty-free License 2.0](https://github.com/slint-ui/slint/blob/master/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md):

[![Made with Slint](https://raw.githubusercontent.com/slint-ui/slint/master/logo/MadeWithSlint-logo-whitebg.png)](https://slint.dev)
