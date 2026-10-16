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
| P11 | Translucent **quick buttons**: ↑ ↓ ← → around OK (Enter), F12 (MiSTer menu), ⌫ (Backspace). Each is shown or hidden on its own and can be **moved anywhere** on the screen (Arrange mode). |
| P12 | The video is a **relative touchpad** (§5.3). |
| P13 | **One MiSTer at a time, full screen**, picked from a list; a **three-finger swipe** switches to the next/previous MiSTer. |
| P14 | **Every on-screen element can be hidden and brought back** (keyboard, function bar, each quick button, control banner, stats); choices are remembered. |
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

Internet access is **opt-in per capture PC**: the setup page's **Internet
access** section has "Allow connections from the internet" (off by default,
`[remote] enabled` in `capture.ini`; turned on or off only on the capture PC
itself), the address phones get, the router's port forward (TCP 5970 to this
PC's home address) and the paired devices. Off: nothing changes; the server
takes connections from the home network only.

On the phone it's per MiSTer: **Use over the internet** in the MiSTer's
settings (long-press it in the list, or swipe), or **Also use over the
internet** when adding it. The phone keeps **one key per capture PC**: each
capture PC has a permanent random id (`server-id` in its config folder, sent
in `Welcome` and `Paired`), and a MiSTer whose capture PC the phone already
has a key for reuses it at once (a stream never connected is asked for its
PC's id first). Otherwise it pairs, once per capture PC, at home:

1. The phone connects to the stream's home address with plain mkvm, as any
   home viewer does.
2. It sends `Pair { device }` (the phone's name) instead of `Hello`. The
   server answers only from a home address, and only when internet access
   is on; otherwise it explains where to turn it on.
3. The server creates a **key id** (16 random bytes, not secret) and a
   **key** (32 random bytes from the OS), saves them in `devices.json` in
   the config folder (name, id, key, created, last seen, last address), and
   answers `paired { key_id, key, internet_address }`.
4. The phone stores the key in the **Keychain** (this device only, not
   synced; readable after the first unlock) and the internet address with
   the host. The core gets the key at launch and never writes it to
   `profiles.json`. If the Keychain refuses, pairing fails rather than
   seeming to work.

The key crosses the home network in the clear once, during pairing: the
home network is trusted anyway (P5). **Pair again** (after a revoke, or a
lost key) sends `Pair.replaces` with the old key id, and the server drops
that entry. The capture PC lists the phone under **this iPhone's name**,
set on the phone (iOS gives apps only "iPhone"; the app asks before the
first pairing, and the gear on the MiSTer list changes it); a paired phone
that connects under a new name is renamed in the list. The same name is what
other viewers see ("Dani's iPhone has control").

`internet_address` is a field on the setup page ("Address from the
internet", e.g. `myhome.duckdns.org:5970`) so it's set once on the PC and
every phone gets it; the phone also lets you type or change it.

The setup page lists **paired devices**: name, paired, last seen and from
where, connected now, **Revoke**. Revoking deletes the key and hangs up that
device's open connections; it can't connect again until it pairs at home.
Turning the phone's switch off forgets the key on the phone (the server's
copy stays until revoked).

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
- An encrypted connection from outside the home network is refused while
  internet access is off, even with a valid key.
- An unknown or revoked key is logged with its address and the connection
  closed after a second's pause (it slows guessing; the key space makes
  guessing hopeless anyway). The phone says the key isn't accepted any more
  and to pair again at home.
- Later, if needed: some routers rewrite the source of forwarded
  connections to their own home address, which would make internet clients
  look local; a `[remote] trust_lan = false` switch could then require
  pairing everywhere.

The phone always uses the secure transport once it has a key, at home too
(same code path, nothing to switch). It tries the route that worked last
first (home or internet), giving it 1.5 s before the other; from the
internet it asks for at most 30 fps at JPEG quality 70 (§4), and the
session screen shows a globe next to the MiSTer's name.

The **predictive cursor** keeps a separate switch and learned model for the
internet (HostProfile `predictive_cursor_internet`, `cursor_*_internet`):
mobile delay is longer and jumpier, so it starts off there (from a 250 ms
guess when turned on) and never overwrites what was learned at home. The
switch is in the controls menu and applies to the route in use.

## 4. Video and audio over the internet

- **Now (MJPEG):** each viewer gets the picture its screen needs (the phone
  sends its video area's size; the server picks the offered size up to 10%
  above it, e.g. 1280x720 in portrait), and from the internet at most
  30 fps at JPEG quality 70 (`Subscribe.max_fps`/`quality`, honoured per
  viewer by the server). Measured on the 720p test pattern: 18.8 Mbit/s
  native → 7 Mbit/s at 30 fps → 3 Mbit/s at 640x360. `misterkvms-cli stream
  --max/--fps/--quality` shows what a connection gets.
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
- **Quick buttons**, translucent (opacity adjustable), three pieces: the
  arrows around **OK** (Enter), **F12** and **⌫** (Backspace). Each is
  shown or hidden on its own. **Move buttons…** in the controls menu enters
  Arrange mode: the screen dims, touches don't reach the MiSTer, each piece
  is outlined and can be dragged anywhere; **Done** keeps the places
  (fractions of the space above the keyboard, so they follow it).
  **Reset positions** puts them back. Outside Arrange mode the buttons never
  move, so pressing one can't drag it by accident. A chevron on the D-pad's
  corner collapses them all into one small round button where the D-pad
  was; tapping it brings them back.
- **MiSTer button** (top left, the open MiSTer's name): a list of the
  saved MiSTers to switch to (the three-finger swipe does the same) and
  **Disconnect**, back to the list.
- **Control banner** ("Watching — *Name* has control", **Take control**),
  as on the Mac.

### 5.3 Touch

| Gesture | Does |
|---|---|
| One finger drag | Move the mouse (relative, with acceleration) |
| Tap | Left click |
| Two-finger tap | Right click |
| Double tap | Double click |
| Tap, then press and drag | Click, then hold the left button while dragging (a single tap waits a moment to see whether a drag follows) |
| Pinch | Zoom the video (pan with two fingers when zoomed) |
| Three-finger swipe left/right | Next / previous MiSTer |
| Three-finger tap | Show/hide the controls menu |

### 5.4 Showing and hiding

A small translucent button in a corner (and the three-finger tap) opens a
menu with a switch per element: keyboard, function bar, control banner,
stats, audio, and each quick button. Hiding everything leaves only the video and touchpad;
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
- `scripts/build-ios.sh --testflight` archives, signs and uploads to App
  Store Connect. Xcode signs in the cloud with the App Store Connect API key
  (`-allowProvisioningUpdates` with the key), so no certificate or profile
  has to be installed anywhere.

### 6.1 CI and TestFlight

The `build-ios` job in `.github/workflows/release.yml` runs with the
others. On a push to `main` (or a manual run on `main`) with the secrets
below set, it uploads the build to TestFlight; otherwise (pull requests, no
secrets) it only builds for the simulator. It isn't part of the GitHub
release, and an iOS failure doesn't hold the desktop release back. The
version is the release's (0.9.N); the build number is the workflow run
number, which only grows, as App Store Connect requires.

One-time setup:

1. **App Store Connect → Apps → + → New App**: iOS, name MiSTerKVMS, bundle
   ID `com.danifunker.misterkvms` (register it under Certificates, IDs &
   Profiles → Identifiers first if it isn't offered), SKU e.g.
   `misterkvms-ios`. Uploads fail until this app record exists.
2. **The API key needs the Admin role** (or "Access to Cloud Managed
   Distribution Certificate") for cloud signing. The notarization key
   (`APPLE_API_KEY_P8`, `APPLE_API_KEY_ID`, `APPLE_API_ISSUER_ID`) is reused;
   if it's a Developer key, the archive step fails with a cloud signing
   permission error: raise its role or create an Admin key and replace the
   three secrets.
3. Repository secret **`APPLE_TEAM_ID`**: the 10-character Team ID
   (developer.apple.com → Membership details).
4. **TestFlight → Internal Testing**: add a group with yourself. Internal
   testers get each build without Apple's review, through the TestFlight
   app, once processing finishes (usually 5–30 minutes).

The app encrypts its own connections with standard algorithms (§3) and
declares that exempt (`ITSAppUsesNonExemptEncryption` = NO in
`ios/Info.plist`, mass market), so uploads aren't held for export
documentation. Declaring YES instead needs export compliance documentation
filed in App Store Connect and its `ITSEncryptionExportComplianceCode` in the
plist; without that code Apple refuses the upload (error 90592). The privacy manifest
(`PrivacyInfo.xcprivacy`) declares no tracking or data collection and the
reasons for the system APIs used (UserDefaults, file timestamps, system
uptime).

## 7. Milestones

| # | Milestone | Done when |
|---|---|---|
| I0 | Mac app check | Mac app builds and works without NDI (IPHONE-HANDOFF.md part 1) |
| I1 | Core on iOS | `kvms-core` builds for device and simulator; `NativeSurface::UiKit`; XCFramework from `build-ios.sh` |
| I2 | iPhone app on the LAN | hosts list, Bonjour + typed address, video, audio, touchpad, keyboard + function bar, remote, show/hide menu, control banner, three-finger switch; runs in the simulator against a real stream |
| I3 | Pairing and secure transport (done) | `mkvm` secure mode with tests; server `devices.json`, pairing, LAN-only plain, Devices on the setup page; core and phone use it |
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
