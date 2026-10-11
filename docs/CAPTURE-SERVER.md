# Capture server plan: `misterkvms-capture` on the RustDesk protocol

Replace NDI with a capture server we own, and use the **RustDesk protocol**
for everything that talks to it: our own apps, stock RustDesk viewers
(desktop, phone, tablet, web), the CLI and an optional OBS plugin.

Decisions so far (October 2026):

| # | Decision |
|---|---|
| R1 | The capture server owns the capture cards; OBS becomes an optional consumer through a plugin. |
| R2 | The RustDesk protocol replaces our own: on the LAN, away from home, and for phones/tablets. No separate MiSTerKVMS protocol. |
| R3 | The whole project moves to **AGPL-3.0** so RustDesk's code can be used anywhere in it (§2). |
| R4 | Downscaling (e.g. to 1080p) is chosen **per viewer** (§5). |
| R5 | NDI stays as an optional, legacy source in the apps. |
| R6 | First, find and fix the client-side memory leak (§1). |

```
Capture PC                                          Viewers (LAN or internet)
┌─────────────────────────────────────────┐
│ misterkvms-capture                      │  RustDesk   ┌───────────────────────────┐
│  capture cards + audio ──▶ per-viewer   │  protocol   │ MiSTerKVMS apps (mac/win/  │
│  scale ──▶ encode (HW H.264/H.265, VP9) │────────────▶│ linux): predictive cursor, │
│  one RustDesk peer (ID) per MiSTer      │             │ integer scale, crop, stats │
│  input from viewers ──▶ core ──▶ mrext ─┼──▶ MiSTer   ├───────────────────────────┤
│                                         │────────────▶│ stock RustDesk viewers:    │
└──────────────▲──────────────────────────┘             │ Windows/mac/Linux/Android/ │
               │ direct IP on the LAN,                  │ iOS/web                    │
               │ rendezvous + relay (hbbs/hbbr) remote  ├───────────────────────────┤
               │                                        │ misterkvms-cli, OBS plugin │
                                                        └───────────────────────────┘
```

## 1. First: the client memory leak

Whatever the video path, the leak has to go first; the new client would
inherit it.

1. **Measure:** `misterkvms-cli soak <minutes>` runs a full `Controller`
   session (input + video, offscreen renderer) and logs RSS, frame counts
   and reconnects every 10 s. Run it on each OS, including against a
   flapping source like the Mac's current NDI link.
2. **Suspects:** allocations per NDI reconnect (frame buffers, audio ring,
   cursor detector); the macOS shell (Metal layers per host switch, the
   AVFoundation recorder, SwiftUI churn from the 2 Hz state events); wgpu
   textures per resolution change; work queued while the view is occluded.
3. **Guard:** a 10-minute soak in CI (Linux, offscreen, synthetic source)
   that fails if memory keeps growing after warm-up.

Done when an 8-hour soak on the Mac holds steady.

## 2. Relicensing to AGPL-3.0

- Replace `LICENSE`, the `license` field in every `Cargo.toml`, the About
  boxes (all three apps) and the README.
- **Mac App Store:** AGPL code can't ship there (Apple's terms conflict with
  it), so decision D15's reason for MIT goes away. Direct-download DMG,
  Windows zip/installer and Linux packages are unaffected.
- **NDI:** the NDI runtime is proprietary. Add an *additional permission*
  (AGPL §7) that allows using the app with the NDI runtime, which is loaded
  at run time and never shipped as part of our code. Same for other
  proprietary system runtimes if needed (e.g. vendor GPU encoders).
- **Network use:** AGPL §13: anyone who offers the capture server to others
  over a network must offer them its source. A "Source" link on the
  server's status page and in the apps' About covers it.
- The OBS plugin (libobs is GPL-2.0-or-later) can combine with AGPL-3.0
  code: the plugin as a whole is distributed under AGPL-3.0.
- All commits so far are by the project owner, so relicensing needs no
  outside consent.

## 3. Names

| Piece | Name |
|---|---|
| Desktop apps | MiSTerKVMS |
| Command-line tool | `misterkvms-cli` (was `kvms`; alias kept for one release) |
| Capture server | `misterkvms-capture` |
| OBS plugin | `obs-misterkvms` |

## 4. The capture server

### 4.1 How to build the RustDesk side

RustDesk's host logic (handshake, login, permissions, video/audio services,
QoS, rendezvous and relay, NAT hole punching) lives in the main `rustdesk`
crate, not a library. Two ways to use it; a spike decides (milestone C3):

- **A. Fork RustDesk as a headless host (preferred).** Pin a release (1.4.9,
  the version in use here) as a submodule and add a "MiSTer" mode:
  - a capturer that reads the capture card instead of the screen (`scrap`
    already abstracts capturers behind a trait),
  - an input sink that sends to mrext through our core instead of
    synthesising OS input (`server/input_service.rs`),
  - no UI; run with a config folder per MiSTer.
  Everything else (codecs, QoS, audio, encryption, rendezvous, relay, peer
  IDs and passwords) comes for free. Cost: rebasing on RustDesk releases.
- **B. Our own host on RustDesk's protocol crates** (`hbb_common`,
  `libs/base`'s `message.proto`, `scrap` codecs). Leaner and under our
  control, but we reimplement the handshake, login and QoS, and keep up
  with protocol changes ourselves.

Either way: **one RustDesk peer (ID + password) per MiSTer**, so each MiSTer
is its own device in a viewer's address book, and viewers of different
MiSTers never interfere. One `misterkvms-capture` process hosts all of them.

### 4.2 Capture

Behind one `CaptureDevice` trait (list devices/formats, open, frames with
timestamps):

| OS | API |
|---|---|
| Windows | Media Foundation `IMFSourceReader` (also gives hardware scaling and colour conversion) |
| macOS | AVFoundation `AVCaptureSession` (needs Camera permission: the server ships as a signed app bundle) |
| Linux | V4L2 |

The spike also tries the `nokhwa` crate, which wraps all three. Audio from
the card's own audio device through `cpal`; each stream pairs a video and an
audio device. A device that drops out or fails to configure (as the UGREEN
did in OBS) is retried with backoff; viewers see a "no signal" frame, not a
disconnect. Resolution changes from the MiSTer are followed automatically.

### 4.3 Encoding

- Hardware H.264/H.265 first (NVENC, Quick Sync, AMF, VideoToolbox, VA-API
  via RustDesk's `hwcodec`), VP9 (software) as the fallback every viewer
  decodes.
- **4:4:4 chroma** where encoder and viewer support it (RustDesk negotiates
  it: `CodecAbility i444`). 4:2:0 smears coloured text and pixel art, which
  is what most MiSTer desktops are.
- Low-latency settings: no B-frames, short GOP, one-frame encoder delay. A
  "LAN" quality preset with a high bitrate cap (RustDesk's quality options
  plus a custom bitrate) for local viewing.

### 4.4 Input

Viewer input arrives as RustDesk `KeyEvent`/`MouseEvent` and goes through
the core's existing input state machine (held keys, release-all, the exit
chord) to mrext:

- **Keyboard:** RustDesk's *Map* mode sends position codes (Windows scan
  code, Linux key code or macOS key code, per the viewer's platform from
  the login). We map all three to HID: Windows and Linux tables exist in
  core; the macOS table moves from Swift to core. *Translate* mode
  (characters, common on phones) goes through `text.rs`.
- **Mouse:** RustDesk's *relative mouse mode* (viewers 1.4.5+) maps
  directly to mrext's relative movement. Absolute positions (older viewers,
  phone touch) are turned into relative moves aimed at the target, using
  the predictive cursor's tracker, which already knows where the guest
  pointer is on screen. Cores that support mrext's absolute `mousePos` get
  that instead.
- **mrext connection:** the server keeps one session per MiSTer. Viewers
  away from home never need to reach the MiSTer directly.

### 4.5 Configuration and running

`capture.json` in the config folder: streams (`name`, video/audio device,
format), each with its MiSTer address (mrext) and RustDesk settings (ID,
permanent password, rendezvous/relay server or LAN-only). Commands:
`misterkvms-capture devices | add | serve | install-service`. A status page
on a local port shows streams, viewers, fps, encoder and drops, with the
"Source" link (§2). Services: Windows service, launchd, systemd.

### 4.6 Away from home

- RustDesk's public servers work out of the box, but **self-hosting**
  `rustdesk-server` (hbbs/hbbr) is recommended: your own key, no
  third-party relay. A Raspberry Pi or the capture PC itself can run it.
- Security becomes required (until now: LAN only, none): per-MiSTer
  permanent password, RustDesk's end-to-end encryption, and an option to
  accept only LAN connections.

## 5. Per-viewer downscaling

Each viewer chooses its resolution:

- The server advertises **SupportedResolutions** per MiSTer: native, plus
  1080p, 720p and the integer fractions of native (4K → 1080p is exactly 2:1).
- A viewer asks with RustDesk's **change resolution** request. Stock
  RustDesk viewers show it in their display menu; our apps set it from a
  per-host "Max resolution" (Native / 1080p / 720p / Auto). Auto picks 1080p
  when the link measures below a threshold.
- The server keeps one scale + encode pipeline per distinct resolution
  (viewers asking for the same size share one). Scaling: exact box filter
  for integer ratios (keeps pixel art crisp), area filter otherwise; on the
  GPU or Media Foundation's video processor where available.
- Encoder load rises with each distinct resolution, so the server caps how
  many it runs at once (configurable; default 3).

## 6. Our apps as RustDesk clients

- The core gets a `RustDeskSource` beside `NdiSource`: connect (direct IP on
  the LAN, rendezvous otherwise), log in, receive video/audio, send input,
  using the same RustDesk crates as the server.
- Decoding: hardware where possible (VideoToolbox, Media Foundation,
  VA-API) through `hwcodec`, VP9 through libvpx. Frames land in the
  existing slot, so the renderer, auto-crop, predictive cursor, recording and
  stats stay as they are.
- Input goes over the same connection, so the apps work away from home too.
  On the LAN the app can keep talking to mrext directly (a per-host option),
  for the lowest input latency.
- Host profiles get a source: `{ kind: "rustdesk", id or host, password }` or
  the legacy `{ kind: "ndi", ... }`. The host editor lists capture-server
  MiSTers found on the LAN and NDI sources.
- **Build cost:** RustDesk's codec stack needs vcpkg-built native libraries
  (libvpx, libyuv, aom, opus) on all three OSes. CI caches them; local
  builds get a setup script.

## 7. OBS plugin `obs-misterkvms`

An OBS **source** ("MiSTerKVMS Capture") that connects to a MiSTer on the
capture server as a RustDesk client and outputs video and audio, so OBS
scenes, recording and streaming keep working while the server owns the card.

- A C plugin shell (from `obs-plugintemplate`, OBS 30+) over a Rust static
  library with a small C API (the same client code as the apps).
- On the same PC as the server it connects over loopback; a shared-memory
  path for raw frames is a later optimisation if decode cost matters.
- Packaging: Windows zip/installer, signed + notarised macOS `.pkg`, Linux
  `.deb` and tarball, from the plugin template's CI.

## 8. Milestones

| # | Milestone | Done when |
|---|---|---|
| C0 | Client leak fixed | 8 h Mac soak steady; CI soak guard |
| C1 | AGPL-3.0 + NDI permission; CLI renamed | licence files, About boxes, README, CI updated |
| C2 | Capture spike | UGREEN (MJPEG) and GC573 (raw) at 1080p60 on Windows via MF or `nokhwa` |
| C3 | RustDesk host spike | fork (A) vs own host (B) decided: a stock RustDesk 1.4.9 viewer shows one MiSTer over direct IP, keyboard (Map mode) and relative mouse reach mrext |
| C4 | `misterkvms-capture` on Windows | several MiSTers, audio, hardware encode, 4:4:4, status page, service |
| C5 | Per-viewer resolution | SupportedResolutions + change resolution; shared pipelines; integer box filter |
| C6 | Away from home | self-hosted hbbs/hbbr tested; passwords; LAN-only option; phone (Android/iOS) and web viewers tested |
| C7 | Apps as RustDesk clients | Mac/Windows/Linux show a server MiSTer with all current features; latency measured against NDI |
| C8 | Server on macOS and Linux | incl. Raspberry Pi 5 as a capture box |
| C9 | OBS plugin | Win/mac/Linux builds; OBS shows a MiSTer with audio |
| C10 | Packaging | server and plugin in releases |

Rough size: C0 a few days; C1–C6 about three weeks; C7–C10 about three more.
The biggest risks are C3 (RustDesk host internals) and C7 (codec build stack
in three apps).

## 9. Risks and open questions

- **Latency vs NDI:** hardware H.264 adds a few milliseconds of encode and
  decode; measure end to end (the predictive cursor hides some of it).
  If the LAN case turns out noticeably worse, raw frames over the protocol's
  `rgb`/`yuv` frame types are an option for local viewers on gigabit, at a
  bandwidth cost.
- **Following RustDesk:** the protocol isn't a published spec and changes
  with releases; pin a version, test stock viewers on every bump.
- **Phones:** RustDesk's mobile viewers send absolute touches; the
  pointer-steering in §4.4 needs testing on a real HP9K/DOS core.
- **macOS capture box:** Camera permission and a signed bundle; capture
  card drivers vary, so the Windows PC stays the realistic capture box.
- **Rendezvous choice:** public RustDesk servers vs self-hosted; affects
  privacy and what the setup guide recommends.
