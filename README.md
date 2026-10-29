# AutoKeyLight

Automatically turns your Elgato lights on and off when your webcam turns on and off.

A cross-platform rewrite in Rust with [Tauri](https://tauri.app) of
[AutoKeyLight](https://github.com/astutejoe/AutoKeyLight) by [Gabriel Garcia (@astutejoe)](https://github.com/astutejoe),
the original Windows app written in C# and WinForms. The idea, the behavior and the icons come from the original project,
which is MIT licensed. Thanks Gabriel!

## Features

- Runs in the tray, click the icon to open the settings window
- Turns the lights on as soon as any app starts streaming from a camera, and off when it stops. Apps that only open
  the camera to list it don't count
- The lights are only switched when the camera turns on or off, so lights you switch yourself stay that way, for
  example turning them on without a call
- Optional delays before turning the lights on or off
- The settings window shows which cameras are in use, and each light's name, IP and whether it's on, off or
  unreachable
- **Track** or **Untrack** each light to choose which ones follow the camera. New lights are tracked
- Event driven, no polling in the background: Windows notifies the app when the camera turns on or off, and lights are
  only asked for their state while the settings window is open, on a separate thread so a slow light never delays
  anything. With the window closed, the app does nothing at all until the camera changes
- If a light doesn't respond when the camera changes, it's retried once a second later, and an unreachable light
  doesn't hold up the others
- Start at login, in the tray
- Closing the window keeps the app running in the tray, **Exit** in the tray menu quits it
- Only one copy runs at a time, starting it again opens the running one
- Finds your lights on the network automatically with mDNS, no IP addresses to enter. Lights that leave the network
  drop off the list, and tracking survives IP changes

## Platform support

| | Windows | macOS | Linux |
|---|---|---|---|
| Tray app and settings | ✅ | Not tested yet | Not tested yet |
| Camera detection | ✅ Media Foundation | Not yet, planned with CoreMediaIO | Not yet |

Without camera detection the app runs but never switches the lights, the settings window says so.

On Windows, camera usage is detected with the Media Foundation
[sensor activity monitor](https://learn.microsoft.com/en-us/windows/win32/api/mfidl/nn-mfidl-imfsensoractivitymonitor).
Windows 11 builds 26200 and newer no longer record camera usage in the registry, which is what older versions of
the original app relied on.

## Finding lights

Elgato lights announce themselves on the local network over mDNS (`_elg._tcp`), and the app lists every light it
finds. Your computer and the lights need to be on the same network, and the firewall has to allow the app on it.
Lights are remembered by the id they announce, their MAC address, so tracking survives the light getting a new IP.

### Windows Firewall

The first time the app starts, Windows Defender Firewall asks whether to allow AutoKeyLight on the network, because
mDNS listens for the lights' announcements. Allow it on **private networks**, the lights won't be found otherwise.
Public networks aren't needed for a home or office network marked as private.

If the prompt was dismissed or access was denied, the light list stays on "Looking for lights on your network…".
To fix it, open **Windows Security** → **Firewall & network protection** → **Allow an app through firewall**, select
**Change settings** and tick **Private** for AutoKeyLight. If it was blocked, Windows also adds block rules for it
under **Advanced settings** → **Inbound Rules**, delete those and start the app again to get the prompt back.

Windows asks again whenever the exe is moved or replaced by a new version at a different path.

Debug builds write a timing log to `autokeylight.log` in the temp directory, showing camera events, discovered lights
and how long each request to a light took.

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
  controller.rs                 event loop that follows the camera and switches the lights
  discovery.rs                  finds lights on the network with mDNS
  lights.rs                     Elgato light HTTP API
  settings.rs                   settings stored as JSON in the app config directory
  log.rs                        timing log for debug builds
  camera/
    mod.rs                      platform independent entry point
    media_foundation.rs         Windows camera detection
ui/                             settings window (plain HTML, CSS and JS, no build step)
```

## License

MIT, see [LICENSE](LICENSE). Includes the copyright notice of the original AutoKeyLight project.
