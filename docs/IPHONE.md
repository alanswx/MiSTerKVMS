# iPhone app plan: MiSTerKVMS for iOS, at home and away

A native iPhone app (App Store, TestFlight first) that watches and controls
a MiSTer through **MiSTerKVMS Capture**, on the home network and from the
internet through one forwarded port. Like the desktop apps it's a thin shell
on the shared Rust core.

Decisions so far (October 2026):

| # | Decision |
|---|---|
| P1 | **iPhone first, portrait first.** iPad and landscape come later from the same code. |
| P2 | Minimum **iOS 17**. |
| P3 | A thin SwiftUI/UIKit shell on the Rust core through UniFFI, like the Mac app. Protocol, pairing, encryption, input mapping and rendering stay in Rust. |
| P4 | The phone connects to **MiSTerKVMS Capture** only (never mrext directly). Away from home: one forwarded **TCP 5970** to the capture PC. The setup page (5971) stays on the LAN. |
| P5 | **The LAN needs no key**, as today: desktop apps and phones at home connect as they do now. |
| P6 | **From the internet, a paired device key is required.** Each device gets its own random 32-byte key when it pairs on the LAN; the setup page lists devices and revokes them one at a time. |
| P7 | The key **both authenticates and encrypts** (Noise, pre-shared key mode, §3). No certificates, nothing to install or trust on the phone. |
| P8 | Plain (unencrypted) mkvm is accepted **from LAN addresses only**. |
| P9 | Internet video is **H.264 at a capped bitrate**, re-encoded on the server (CAPTURE-SERVER.md C8), decoded in hardware on the phone. |
| P10 | Keyboard: the **iOS system keyboard** plus a **function bar** above it (Esc, Tab, Ctrl, Alt, Win, F1–F12). No custom keyboard. |
| P11 | A floating, translucent, collapsible **remote**: ↑ ↓ ← →, OK (Enter), F12 (MiSTer menu), ⌫ (Backspace). |
| P12 | The video is a **relative touchpad** (§5.3). |
| P13 | **One MiSTer at a time, full screen**, picked from a list; a **three-finger swipe** switches to the next/previous MiSTer. |
| P14 | **Every on-screen element can be hidden and brought back** (keyboard, function bar, remote, control banner, stats); choices are remembered. |
| P15 | Bundle ID **com.danifunker.misterkvms**. TestFlight first, App Store after. |
| P16 | Discovery with **Bonjour** (`NWBrowser`, `_mkvm._tcp`, which the server already advertises from Windows); typing an address always works. |
| P17 | Not in the first version: hardware keyboard/mouse/game controller, the paste box, iPad, landscape. |

```
Home                                                    Phone
┌───────────────────────────────┐   LAN: plain mkvm or     ┌───────────────────┐
│ misterkvms-capture            │◀──── secure mkvm ───────▶│ MiSTerKVMS (iOS)  │
│  TCP 5970  mkvm               │                          │  Rust core        │
│   plain:  LAN addresses only  │   Internet: secure mkvm  │   (UniFFI)        │
│   secure: any address, needs  │◀── via router :5970 ────▶│  key in Keychain  │
│           a paired device key │                          └───────────────────┘
│  devices.json (paired keys)   │
│  setup page :5971 (LAN only)  │
└───────────────────────────────┘
```

## 1. Code layout

| Where | What |
|---|---|
| `mkvm/` | Gains the secure transport (§3): handshake, framing inside Noise, and the `pair` messages. Pure logic with tests, like the rest of the crate. |
| `server/` | `devices.json`, pairing, LAN-only plain connections, secure connections, Devices section on the setup page. H.264 encode (C8). |
| `codec/` | H.264 encode (server) and decode (VideoToolbox on Apple). |
| `core/` | Builds for `aarch64-apple-ios` and `aarch64-apple-ios-sim`; `NativeSurface::UiKit`; secure connections with a device key; pairing call. No local capture on iOS. |
| `ios/` | The iPhone app: Xcode project, SwiftUI/UIKit sources. Uses the same UniFFI bindings (`KVMSCore`) as `apple/`. |
| `scripts/build-ios.sh` | Rust for device + simulator → `KVMSCore.xcframework` → bindings → `xcodebuild`. |

## 2. Pairing

Pairing happens once, at home, on the LAN:

1. The phone finds the stream (Bonjour, or a typed `host[:port]/stream`) and
   connects with plain mkvm, as any LAN viewer does.
2. It sends `pair { device }` (the phone's name). The server only answers
   this from a LAN address.
3. The server creates a **key id** (16 random bytes, not secret) and a
   **key** (32 random bytes from the OS), saves them in `devices.json` in
   the config folder (name, id, key, created, last seen, last address), and
   answers `paired { key_id, key, internet_address }`.
4. The phone stores the key in the **Keychain** (this device only, not
   synced) and the internet address with the host.

`internet_address` is a field on the setup page ("Address from the
internet", e.g. `myhome.duckdns.org:5970`) so it's set once on the PC and
every phone gets it; the phone also lets you type or change it.

The setup page gets a **Devices** section: name, paired, last seen, last
address, **Revoke**. Revoking deletes the key and drops that device's open
connections.

Any LAN device may pair without approval: the LAN already has full access
(P5), so an approval step would add nothing. If that changes, a
`[remote] approve_pairing` switch can hold requests until clicked on the
setup page.

## 3. Secure transport

On the same TCP port 5970, told apart by the first bytes:

1. The phone sends a short clear prelude: magic `MKVS`, version, key id.
2. The server looks the key id up. Unknown or revoked: it closes.
3. Both run a **Noise `NNpsk0_25519_ChaChaPoly_BLAKE2s`** handshake (the
   `snow` crate) with the device key as the pre-shared key. Two messages,
   one round trip. Only a holder of the key can complete it; each connection
   gets fresh session keys (forward secrecy), so a recorded session can't be
   decrypted later even with the key.
4. After that, the usual mkvm conversation (`hello`, `welcome`, video…)
   runs inside Noise transport messages, split into chunks of at most
   64 KB. ChaCha20-Poly1305 costs nothing noticeable at these bitrates.

Why not TLS: TLS wants certificates, which for a home server means
self-signed ones and pinning. With a per-device key we already have a shared
secret, and Noise with a pre-shared key is the standard way to use one:
small, well studied (WireGuard uses the same design), no certificates.

Server rules:

- Plain mkvm from a non-LAN address is refused ("this server needs a paired
  device from outside the home network").
- LAN means loopback, RFC 1918, link-local, CGNAT `100.64/10` excluded
  (that's a carrier's network), IPv6 ULA and link-local.
- Some routers and reverse proxies rewrite the source of forwarded
  connections to their own LAN address, which would make internet clients
  look local. `[remote] trust_lan = false` in `capture.ini` turns the LAN
  exception off (then every viewer must pair). The setup page warns when
  remote access is on and a connection on the forwarded port arrives from
  the router's own address.
- A failed handshake is logged with its address; repeated failures from one
  address are delayed (simple back-off, not a lockout).

The phone always uses the secure transport once it has a key, at home too
(same code path, nothing to switch).

## 4. Video and audio over the internet

- **H.264** (C8): Media Foundation hardware encoder on Windows, `openh264`
  as fallback; no B-frames, short GOP, keyframe on request. `Subscribe`
  gains `max_bitrate` (optional, so old viewers are unaffected).
- Phone presets: **Auto** (default), **Balanced** 1080p ~6 Mbit/s,
  **Low** 720p ~2.5 Mbit/s, **Best** (native MJPEG, LAN only). Auto starts at
  Balanced away from home and Best on the LAN, and steps down on rising
  frame age or drops (CAPTURE-SERVER.md §6).
- **Decode** in `codec` with VideoToolbox (shared with the Mac app, which
  gains H.264 too), into the core's frame slot; the wgpu renderer is
  unchanged.
- **Audio** stays 48 kHz s16 PCM at first: about 1.5 Mbit/s. An audio
  on/off switch covers slow connections; Opus is a later improvement.
- TCP for now; QUIC (CAPTURE-SERVER.md §4) later for lossy connections.

## 5. The app

### 5.1 Hosts

- A list of MiSTers (one row per stream), with state (at home / away /
  unreachable) and Paired or Not paired.
- **Add:** streams found on the network, or a typed address. Opening one at
  home pairs it automatically (§2).
- Each host keeps a LAN address and an internet address. Connecting tries
  the LAN address first (short timeout) and the internet address alongside,
  and uses whichever answers first, preferring the LAN.
- Hosts live in the core's `profiles.json` in the app's Application
  Support folder; keys only in the Keychain, handed to the core when
  connecting.

### 5.2 Session screen (portrait)

Full screen, status bar and home indicator hidden. From top:

- **Video**, aspect-fit, across the full width.
- **Touchpad area**: the video and the space below it down to the keyboard.
- **Function bar** above the keyboard: Esc, Tab, Ctrl, Alt, Win (sticky:
  tap = next key only, double tap = locked), then F1–F12 scrolling.
- **System keyboard**. Typed characters become key presses through the
  core's US-layout text mapping (`core/src/text.rs`); Backspace and Return
  are keys.
- **Remote**: floating, translucent, draggable. D-pad with **OK** (Enter)
  in the middle, **F12** and **⌫** (Backspace) below. Collapses to a small
  round button; opacity adjustable.
- **Control banner** ("Watching — *Name* has control", **Take control**),
  as on the Mac.

### 5.3 Touch

| Gesture | Does |
|---|---|
| One finger drag | Move the mouse (relative, with acceleration) |
| Tap | Left click |
| Two-finger tap | Right click |
| Double tap and drag | Hold the left button while dragging |
| Pinch | Zoom the video (pan with two fingers when zoomed) |
| Three-finger swipe left/right | Next / previous MiSTer |
| Three-finger tap | Show/hide the controls menu |

### 5.4 Showing and hiding

A small translucent button in a corner (and the three-finger tap) opens a
menu with a switch per element: keyboard, function bar, remote, control
banner, stats, audio. Hiding everything leaves only the video and touchpad;
the three-finger tap always brings the menu back. Choices are remembered for
the app (not per host).

### 5.5 iOS specifics

- `NSLocalNetworkUsageDescription` and `NSBonjourServices` (`_mkvm._tcp`).
  No multicast entitlement: discovery uses `NWBrowser`; the core's
  `mdns-sd` discovery is compiled in but not called on iOS.
- `AVAudioSession` category playback, set up by the app before audio starts.
- The connection closes when the app goes to the background and reconnects
  when it comes back (iOS suspends sockets anyway); control is released on
  the way out.
- Network changes (Wi-Fi ↔ cellular) reconnect, picking LAN or internet
  again.

## 6. Building

- Rust targets `aarch64-apple-ios` and `aarch64-apple-ios-sim` (rustup).
- `scripts/build-ios.sh`: builds `libkvms_core.a` for both, wraps them in
  `KVMSCore.xcframework`, generates the Swift bindings, and runs
  `xcodebuild` for the simulator or a device.
- The Xcode project in `ios/` uses folder references (Xcode 16), so adding
  Swift files doesn't change the project file.
- CI builds iOS only once it builds locally. TestFlight uploads use the App
  Store Connect API key the release workflow already has for notarization.

## 7. Milestones

| # | Milestone | Done when |
|---|---|---|
| I0 | Mac app check | Mac app builds and works without NDI (IPHONE-HANDOFF.md part 1) |
| I1 | Core on iOS | `kvms-core` builds for device and simulator; `NativeSurface::UiKit`; XCFramework from `build-ios.sh` |
| I2 | iPhone app on the LAN | hosts list, Bonjour + typed address, video, audio, touchpad, keyboard + function bar, remote, show/hide menu, control banner, three-finger switch; runs in the simulator against a real stream |
| I3 | Pairing and secure transport | `mkvm` secure mode with tests; server `devices.json`, pairing, LAN-only plain, Devices on the setup page; core and phone use it |
| I4 | H.264 (C8) | server encodes (Windows hardware + openh264), Apple decodes with VideoToolbox; bitrate cap; Auto quality |
| I5 | Away from home | phone on cellular through a forwarded port: picture, sound, input; switching Wi-Fi ↔ cellular reconnects |
| I6 | TestFlight | icon, privacy strings, export compliance answered, build uploaded |
| Later | iPad, landscape, hardware keyboard/mouse/controller, paste box, Opus audio, QUIC, pairing by QR code from outside the LAN | |

## 8. Risks and open questions

- **H.264 on the server** is the largest piece (C8); until it lands, away
  from home only works on fast connections (MJPEG is 36–120 Mbit/s).
- **Source-rewriting routers** would make internet clients look local
  (§3: `trust_lan`, and the warning).
- **System keyboard limits:** it sends characters, not key presses, so
  there's no key-hold or auto-repeat from it, and non-US layouts map through
  characters. The function bar and remote cover held keys only as taps.
  A hardware keyboard (later) gives real key presses.
- **App Review:** a remote-control app for the user's own hardware is
  normal, but review needs a way to try it; a demo stream (the server's test
  pattern on a public address) may be needed.
- **Export compliance:** the app uses encryption (X25519, ChaCha20-Poly1305)
  not provided by iOS, so App Store Connect asks about it; this kind of use
  is normally exempt but has to be declared.
