# MiSTerKVMS

A KVM-style desktop app for [MiSTer FPGA](https://github.com/MiSTer-devel/Main_MiSTer/wiki): keyboard and mouse go to MiSTer through [mrext Remote](https://github.com/wizzomafizzo/mrext), and video/audio come from OBS Studio over NDI®.

Status: 0.9 (early). Keyboard/mouse via mrext, NDI video and audio, scaling/zoom/auto-crop, recording to MP4, and an experimental predictive cursor that hides the capture lag. See [docs/PLANNING.md](docs/PLANNING.md), [docs/SETUP-NDI.md](docs/SETUP-NDI.md), and the Windows/Linux port plans in [docs/PORTING.md](docs/PORTING.md).

## Install

From [Releases](https://github.com/danifunker/MiSTerKVMS/releases):

- **macOS:** open the DMG and drag **MiSTerKVMS** to **Applications**. Universal app (Apple Silicon + Intel), macOS 14 or later.
- **Linux:** unpack `MiSTerKVMS-<version>-linux-x86_64.tar.gz` and run `./install.sh` (adds it to the app menu) or `./misterkvms` in place. Needs GTK 4.14+ and libadwaita 1.5+ (Ubuntu 24.04, Fedora 40 or newer). NDI discovery needs `avahi-daemon`.
- **Windows:** unzip `MiSTerKVMS-<version>-win-x64.zip` anywhere and run **MiSTerKVMS.exe**. 64-bit Windows 10 or 11; nothing else to install for the app itself. It isn't code-signed yet, so SmartScreen may warn the first time (More info → Run anyway). The first time video starts, allow MiSTerKVMS through Windows Firewall on private networks.

You also need:

- [NDI Tools](https://ndi.video/tools/) (or the NDI Runtime) on this computer.
- [mrext Remote](https://github.com/wizzomafizzo/mrext) running on each MiSTer.
- OBS Studio with the [DistroAV](https://github.com/DistroAV/DistroAV) NDI plugin on the PC with the capture card(s): see [docs/SETUP-NDI.md](docs/SETUP-NDI.md).

On first use macOS asks for **Accessibility** (to send ⌘Tab and other system shortcuts to MiSTer) and, if you enable raw mouse input, **Input Monitoring**. Windows needs no permissions. Recording on Windows needs [ffmpeg](https://ffmpeg.org) on the PATH (`winget install ffmpeg`).

## Layout

- `core/`: Rust core, shared by every platform: the `Controller` (hosts, the live session, stats, recording), mrext WebSocket client, keymaps, NDI, the wgpu renderer, audio, and Windows input capture. Hosts are saved in `profiles.json` in `~/Library/Application Support/MiSTerKVMS` or `%APPDATA%\MiSTerKVMS`.
- `apple/`: macOS shell (SwiftUI + AppKit), using the core through UniFFI bindings.
- `windows/`: Windows shell (WinUI 3, C#/.NET 10), using the core through its small C interface (`core/src/capi.rs`).
- `linux/`: Linux shell (GTK 4 + libadwaita, Rust), calling the core directly. Settings in `~/.config/misterkvms`.
- `uniffi-bindgen/`: generates the Swift bindings.

## Build (macOS)

Requires Rust and Xcode 16+.

```bash
scripts/build-mac.sh --run
```

The app is written to `build/MiSTerKVMS.app`. It's signed with your "Apple Development" certificate so macOS remembers the Accessibility permission between builds. Set `SIGN_IDENTITY=-` for ad-hoc signing.

For a universal build (needs rustup with `aarch64-apple-darwin` and `x86_64-apple-darwin`): `UNIVERSAL=1 scripts/build-mac.sh`. Package a drag-to-install DMG with `scripts/package-dmg.sh build/MiSTerKVMS.dmg`.

## Build (Linux)

Requires Rust and, on Ubuntu/Debian, `sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev libasound2-dev`.

```bash
scripts/build-linux.sh --run
```

It also writes `build/MiSTerKVMS-<version>-linux-<arch>.tar.gz`. For video, install the NDI SDK/runtime for Linux (`libndi.so.6`).

## Build (Windows)

Requires Rust (MSVC toolchain), the .NET 10 SDK and the NDI Runtime.

```powershell
scriptsuild-windows.ps1 -Run
```

`-Package` publishes a self-contained build and zips it to `build\MiSTerKVMS-<version>-win-x64.zip`. The app logs to `%LOCALAPPDATA%\MiSTerKVMS\MiSTerKVMS.log`.

Core tests: `cargo test -p kvms-core`.

The tools in `core/examples` (connection probe, NDI probe, latency and predictive-cursor checks) read your MiSTer address and NDI source from `local.settings.json` in the repo root. Copy `local.settings.example.json` to start; the real file is git-ignored so your network details stay out of the repo. Arguments on the command line override it. For example, `cargo run -p kvms-core --example probe` connects and prints status without sending any input.

## Using it

- **Capture** starts when a host connects and whenever you click the screen.
- **Release**: **Right ⌘ + Right ⌥ + Esc** (Windows: **Right Win + Right Alt + Esc**; Linux: **Right Super + Right Alt + Esc**). Switching to another window also releases.
- **F12** opens the MiSTer menu.
- **⌥-click** (Windows: **Alt-click**) sends a middle click (configurable per host).
- Windows never lets an app capture **Ctrl+Alt+Del** or **Win+L**: use **Send Ctrl+Alt+Del** in the toolbar's **…** menu.

## Command-line tool (`misterkvms-cli`)

`misterkvms-cli` (formerly `kvms`, which still works for this release) drives a MiSTer from scripts, for testing and debugging cores: screenshots, recordings, typing, key combos, mouse, and loading a freshly built core. It ships inside the app (on Windows, `misterkvms-cli.exe` next to `MiSTerKVMS.exe`); on macOS, to put it on your PATH:

```bash
sudo ln -sf /Applications/MiSTerKVMS.app/Contents/MacOS/misterkvms-cli /usr/local/bin/misterkvms-cli
```

It reads the MiSTer address and NDI source from `local.settings.json` (searched from the current folder upwards; or `--settings`, `--mister`, `--ndi`). Every command prints one line of JSON.

| Command | What it does |
|---|---|
| `misterkvms-cli status` | Core/game running, mrext and NDI reachable |
| `misterkvms-cli screenshot out.png` | Current picture from the capture, borders cropped (`--full` keeps them) |
| `misterkvms-cli screenshot --native out.png` | MiSTer's own screenshot: the core's native resolution, pixel-exact (also saved on the MiSTer) |
| `misterkvms-cli record 10s out.mp4` | Video + audio (needs `ffmpeg`); `out.wav` for audio only |
| `misterkvms-cli frames 5s dir/` | Every changed frame as PNG (`--all` keeps repeats) |
| `misterkvms-cli type 'LOAD "*",8,1\n'` | Type text (US layout); `--delay` / `--hold` in ms for slow machines |
| `misterkvms-cli key ctrl+alt+del` | Key or combo; `misterkvms-cli key menu` opens the MiSTer menu (F12) |
| `misterkvms-cli mouse move 10 -5` / `misterkvms-cli mouse click right` | Mouse |
| `misterkvms-cli wait-for-change --timeout 30s` | Block until the screen changes; `--until-still 2s` waits until it settles |
| `misterkvms-cli load-core build/MyCore.rbf` | Copy over SSH to `/media/fat/_Development` and load it |
| `misterkvms-cli sources` | List NDI sources |

A typical test loop: `misterkvms-cli load-core out.rbf && misterkvms-cli wait-for-change --until-still 3s --timeout 60s && misterkvms-cli type 'dir\n' && misterkvms-cli screenshot --native result.png`.

Several MiSTers can be named under `machines` and picked with `--host` (or `-H`); `default` is used when you don't pass one, and `misterkvms-cli machines` lists them:

```json
{
  "default": "mister1",
  "obs_host": "obs-pc.local",
  "machines": {
    "mister1": { "mister_host": "192.168.1.50", "ndi_source": "OBSPC (MiSTer-1)" },
    "mister2": { "mister_host": "192.168.1.51", "ndi_source": "OBSPC (MiSTer-2)" }
  }
}
```

`misterkvms-cli --host mister2 screenshot shot.png`. A machine's entries override the top-level ones. Other optional settings (top level or per machine): `obs_host` (IP or name; the OBS PC is then asked directly, for networks where discovery can't find it), `ssh_user` (default `root`), `core_dest`, `type_delay_ms`, `type_hold_ms`. See `local.settings.example.json`.

## Releases

Every push to `main` builds a universal DMG, a Windows zip and a Linux tarball and publishes a GitHub release (`.github/workflows/release.yml`). The version is MAJOR.MINOR from `Cargo.toml` plus an auto-incremented patch (0.9.0, 0.9.1, …); bump `Cargo.toml` to start a new series. Signing and notarization use these repository secrets when present:

| Secret | What |
|---|---|
| `MACOS_CERT_P12` | Developer ID Application certificate + private key, exported as .p12, base64 |
| `MACOS_CERT_PASSWORD` | Password of that .p12 |
| `APPLE_API_KEY_P8` | App Store Connect API key (.p8), base64 |
| `APPLE_API_KEY_ID` | Its key ID |
| `APPLE_API_ISSUER_ID` | Its issuer ID |

Without them the release is ad-hoc signed and not notarized.

## Credits

- **MiSTer-Kun** mascot (in the app icon): original design by HeWhoisRed, a gift to the MiSTer community; remastered artwork from [baxysquare/mister_kun](https://github.com/baxysquare/mister_kun). See [apple/Icon/CREDITS.md](apple/Icon/CREDITS.md).
- [MiSTer FPGA](https://github.com/MiSTer-devel/Main_MiSTer/wiki), [mrext](https://github.com/wizzomafizzo/mrext), [OBS Studio](https://obsproject.com) and [DistroAV](https://github.com/DistroAV/DistroAV).

## License

MIT. NDI® is a registered trademark of Vizrt NDI AB; see <https://ndi.video>.
