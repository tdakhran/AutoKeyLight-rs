# AutoKeyLight

Automatically turns your Elgato lights on and off when your webcam turns on and off.

A cross-platform rewrite in Rust with [Tauri](https://tauri.app) of
[AutoKeyLight](https://github.com/astutejoe/AutoKeyLight) by [Gabriel Garcia (@astutejoe)](https://github.com/astutejoe),
the original Windows app written in C# and WinForms. The idea, the behavior and the icons come from the original project,
which is MIT licensed. Thanks Gabriel!

## Features

- Runs in the tray, the settings window opens from the tray icon
- Turns the lights on when any app starts streaming from a camera, and off when it stops
- Optional delays before turning the lights on or off
- **Allow manual override**: only switch the lights when the camera turns on or off, so lights you turn on yourself stay on
- Start at login
- Only one copy runs at a time, starting it again opens the running one
- Imports your lights from Elgato Control Center on first run (Windows)

## Platform support

| | Windows | macOS | Linux |
|---|---|---|---|
| Tray app and settings | ✅ | ✅ | ✅ |
| Camera detection | ✅ Media Foundation | Planned (CoreMediaIO) | Planned |

On Windows, camera usage is detected with the Media Foundation
[sensor activity monitor](https://learn.microsoft.com/en-us/windows/win32/api/mfidl/nn-mfidl-imfsensoractivitymonitor).
Windows 11 builds 26200 and newer no longer record camera usage in the registry, which is what older versions of
the original app relied on.

## Adding lights

Add each light by its IP address, for example `192.168.1.20`. The default port `9123` is used unless you add one
yourself, for example `192.168.1.20:9123`. You can find the IP in Elgato Control Center under the light's settings.

Giving your lights fixed IPs in your router keeps them from changing.

## Building

Install [Rust](https://rustup.rs) and the [Tauri prerequisites](https://tauri.app/start/prerequisites/) for your
platform, then:

```sh
cargo build --release
```

The binary ends up in `target/release/`.

To build installers (`.msi`/`.exe`, `.dmg`, `.deb`/AppImage), install the Tauri CLI and run it on the target platform:

```sh
cargo install tauri-cli --version "^2"
cargo tauri build
```

### Building natively on Windows

The MSVC toolchain is the one Tauri officially supports, use it for releases. You need the Visual Studio C++ build
tools with a Windows SDK (the "Desktop development with C++" workload), then in PowerShell:

```powershell
winget install Rustlang.Rustup
# open a new terminal so cargo is on the PATH
cargo build --release
```

The exe ends up in `target\release\autokeylight.exe`. If the code lives in WSL, clone it to a Windows drive first,
building from a `\\wsl.localhost\...` path is slow and some build scripts don't handle it.

### Cross-compiling for Windows from Linux

With the MinGW toolchain installed (`mingw-w64-gcc` on Arch, `mingw-w64` on Debian/Ubuntu):

```sh
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
```

The exe ends up in `target/x86_64-pc-windows-gnu/release/autokeylight.exe`. It needs the WebView2 runtime,
which comes with Windows 11.

## Project layout

```
src/
  main.rs                       tray, window and commands for the settings UI
  controller.rs                 loop that follows the camera and switches the lights
  lights.rs                     Elgato light HTTP API
  settings.rs                   settings stored as JSON in the app config directory
  camera/
    mod.rs                      platform independent entry point
    media_foundation.rs         Windows camera detection
ui/                             settings window (plain HTML, CSS and JS, no build step)
```

## License

MIT, see [LICENSE](LICENSE). Includes the copyright notice of the original AutoKeyLight project.
