# AutoKeyLight

Automatically turns your Elgato lights on and off when your webcam turns on and off.

A cross-platform rewrite in Rust with [Tauri](https://tauri.app) of
[AutoKeyLight](https://github.com/astutejoe/AutoKeyLight) by [Gabriel Garcia (@astutejoe)](https://github.com/astutejoe),
the original Windows app written in C# and WinForms. The idea, the behavior and the icons come from the original project,
which is MIT licensed. Thanks Gabriel!

## Features

- Runs in the tray (the menu bar on macOS), click the icon to open the settings window
- Turns the lights on as soon as any app starts streaming from a camera, and off when it stops. Apps that only open
  the camera to list it don't count
- The lights are only switched when the camera turns on or off, so lights you switch yourself stay that way, for
  example turning them on without a call
- Optional delays before turning the lights on or off
- The settings window shows which cameras are in use, and each light's name, IP and whether it's on, off or
  unreachable. It sizes itself to its content
- **Track** or **Untrack** each light to choose which ones follow the camera. New lights are tracked
- Event driven, no polling in the background: the system notifies the app when the camera turns on or off, and lights are
  only asked for their state while the settings window is open, on a separate thread so a slow light never delays
  anything. With the window closed, the app does nothing at all until the camera changes
- If a light doesn't respond when the camera changes, it's retried once a second later, and an unreachable light
  doesn't hold up the others
- Start at login, in the tray
- Closing the window keeps the app running in the tray, **Exit** in the tray menu quits it
- Only one copy runs at a time, starting it again opens the running one
- Finds your lights on the network automatically with mDNS, no IP addresses to enter. Lights that leave the network
  drop off the list, and tracking survives IP changes
- **About** in the tray menu shows the version, author and a link to GitHub, the window shows the version and a link to
  GitHub at the bottom

## Installing

Download the installer for your system from the [releases](https://github.com/tdakhran/AutoKeyLight-rs/releases).

**Windows:** run `AutoKeyLight_<version>_x64-setup.exe`. It installs for your user only, no administrator rights
needed, into `%LOCALAPPDATA%\AutoKeyLight`, and adds a Start menu shortcut. To uninstall, use **Settings** → **Apps**,
which also removes the start at login entry. Tick **Delete the application data** in the uninstaller to remove your
settings too.

**macOS:** open `AutoKeyLight_<version>_aarch64.dmg` and drag AutoKeyLight into Applications. The app lives in the menu
bar, without a Dock icon. Builds aren't signed with an Apple Developer certificate, so the first time, right click the
app and choose **Open**, or allow it under **System Settings** → **Privacy & Security**. To uninstall, turn off start
at login in the app first, then drag it from Applications to the Trash. Settings are kept in
`~/Library/Application Support/com.tdakhran.autokeylight`. Only Apple silicon Macs are supported for now.

## Platform support

| | Windows | macOS | Linux |
|---|---|---|---|
| Tray app and settings | ✅ | ✅ | Not tested yet |
| Camera detection | ✅ Media Foundation | ✅ CoreMediaIO | Not yet |
| Tested on | Windows 11 26300 | macOS 26.4, Apple silicon | |

Without camera detection the app runs but never switches the lights, the settings window says so.

On Windows, camera usage is detected with the Media Foundation
[sensor activity monitor](https://learn.microsoft.com/en-us/windows/win32/api/mfidl/nn-mfidl-imfsensoractivitymonitor).
Windows 11 builds 26200 and newer no longer record camera usage in the registry, which is what older versions of
the original app relied on.

On macOS, the app listens to each camera's "running somewhere" property in
[CoreMediaIO](https://developer.apple.com/documentation/coremediaio), which is set while any app streams from it.
Cameras that are plugged in later are picked up too. macOS doesn't say which app is using the camera, only that one is.

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

### macOS Local Network permission

macOS asks whether AutoKeyLight may **find and connect to devices on your local network** the first time it looks for
lights. Click **Allow**, the lights won't be found otherwise. If it was denied, turn AutoKeyLight on under
**System Settings** → **Privacy & Security** → **Local Network**.

The permission belongs to the exact binary, so builds that aren't code signed ask again after every rebuild.

## Debug log

Debug builds write a timing log to `autokeylight.log` in the temp directory (`%TEMP%` on Windows, `$TMPDIR` on macOS),
showing camera events, discovered lights and how long each request to a light took.

## Building

Install [Rust](https://rustup.rs) and the [Tauri prerequisites](https://tauri.app/start/prerequisites/) for your
platform, then:

```sh
cargo build --release
```

The binary ends up in `target/release/`.

To build the installers, install the Tauri CLI and run it on the target platform. Windows builds a setup `.exe`
(NSIS), macOS builds `AutoKeyLight.app` and a `.dmg`:

```sh
cargo install tauri-cli --version "^2"
cargo tauri build
```

The installers end up in `target/release/bundle/`. Their name, publisher, description and icons are set under
`bundle` in `tauri.conf.json`, `Info.plist` adds the macOS specific keys and `windows/installer-hooks.nsh` cleans up
the start at login entry when uninstalling on Windows.

### Version and author

The version, author and GitHub link shown in the app and the installers all come from `Cargo.toml`. To release a new
version, bump `version` there.

### Releases

[GitHub Actions](.github/workflows/build.yml) builds the Windows installer and the macOS disk image on every push and
pull request, they're attached to each run. Pushing a tag that matches the version, for example `v0.2.0`, also
creates a draft release with both installers, publish it on GitHub when it looks right:

```sh
git tag v0.2.0
git push origin v0.2.0
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

### Building on macOS

Install the Xcode Command Line Tools and Rust, then build:

```sh
xcode-select --install
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
cargo build --release
```

`cargo tauri build` creates `AutoKeyLight.app` and a `.dmg`. Set `CI=true` when building over SSH, styling the disk
image window goes through Finder, which needs a logged in desktop.

To try a build over SSH, a user has to be logged in to the desktop, and the app has to be started inside that session
with `open`, which needs an app bundle. A minimal one is enough: the binary in `AutoKeyLight.app/Contents/MacOS/`
and an `Info.plist` with `CFBundleExecutable` set to `autokeylight` and `LSUIElement` set to true, so the app stays
out of the Dock. Apps started over SSH can't use the camera themselves, but `open -a "Photo Booth"` streams from it
in the desktop session.

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
    core_media_io.rs            macOS camera detection
ui/                             settings window (plain HTML, CSS and JS, no build step)
windows/installer-hooks.nsh     Windows uninstaller cleanup
Info.plist                      macOS bundle keys, merged into the app's Info.plist
.github/workflows/build.yml     builds the installers and draft releases
```

## License

MIT, see [LICENSE](LICENSE). Includes the copyright notice of the original AutoKeyLight project.
