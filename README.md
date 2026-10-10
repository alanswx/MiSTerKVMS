# MiSTerKVMS

A KVM-style desktop app for [MiSTer FPGA](https://github.com/MiSTer-devel/Main_MiSTer/wiki): keyboard and mouse go to MiSTer through [mrext Remote](https://github.com/wizzomafizzo/mrext), and video/audio come from OBS Studio over NDI®.

Status: 0.9 (early). Keyboard/mouse via mrext, NDI video and audio, scaling/zoom/auto-crop, recording to MP4, and an experimental predictive cursor that hides the capture lag. See [docs/PLANNING.md](docs/PLANNING.md) and [docs/SETUP-NDI.md](docs/SETUP-NDI.md).

## Install

Download the latest DMG from [Releases](https://github.com/danifunker/MiSTerKVMS/releases), open it and drag **MiSTerKVMS** to **Applications**. Universal app (Apple Silicon + Intel), macOS 14 or later.

You also need:

- [NDI Tools](https://ndi.video/tools/) (or the NDI Runtime) on the Mac.
- [mrext Remote](https://github.com/wizzomafizzo/mrext) running on each MiSTer.
- OBS Studio with the [DistroAV](https://github.com/DistroAV/DistroAV) NDI plugin on the PC with the capture card(s): see [docs/SETUP-NDI.md](docs/SETUP-NDI.md).

On first use macOS asks for **Accessibility** (to send ⌘Tab and other system shortcuts to MiSTer) and, if you enable raw mouse input, **Input Monitoring**.

## Layout

- `core/`: Rust core (sessions, mrext WebSocket client, keymaps). Shared by every platform.
- `apple/`: macOS shell (SwiftUI + AppKit), using the core through UniFFI bindings.
- `uniffi-bindgen/`: generates the Swift bindings.

## Build (macOS)

Requires Rust and Xcode 16+.

```bash
scripts/build-mac.sh --run
```

The app is written to `build/MiSTerKVMS.app`. It's signed with your "Apple Development" certificate so macOS remembers the Accessibility permission between builds. Set `SIGN_IDENTITY=-` for ad-hoc signing.

For a universal build (needs rustup with `aarch64-apple-darwin` and `x86_64-apple-darwin`): `UNIVERSAL=1 scripts/build-mac.sh`. Package a drag-to-install DMG with `scripts/package-dmg.sh build/MiSTerKVMS.dmg`.

Core tests: `cargo test -p kvms-core`.

The tools in `core/examples` (connection probe, NDI probe, latency and predictive-cursor checks) read your MiSTer address and NDI source from `local.settings.json` in the repo root. Copy `local.settings.example.json` to start; the real file is git-ignored so your network details stay out of the repo. Arguments on the command line override it. For example, `cargo run -p kvms-core --example probe` connects and prints status without sending any input.

## Using it

- **Capture** starts when a host connects and whenever you click the screen.
- **Release**: **Right ⌘ + Right ⌥ + Esc**.
- **F12** opens the MiSTer menu.
- **⌥-click** sends a middle click (configurable per host).

## Releases

Every push to `main` builds a universal DMG and publishes a GitHub release (`.github/workflows/release.yml`). The version is MAJOR.MINOR from `Cargo.toml` plus an auto-incremented patch (0.9.0, 0.9.1, …); bump `Cargo.toml` to start a new series. Signing and notarization use these repository secrets when present:

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
