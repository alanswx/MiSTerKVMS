# MiSTerKVMS — Planning

A native desktop app (macOS first, cross-platform later) that works like a KVM for MiSTer FPGAs on the LAN:

- **Input**: keyboard and mouse go to MiSTer through **mrext Remote** (`ws://<mister>:8182/api/ws`).
- **Video + audio**: come from **OBS Studio** on a separate PC with the capture card(s), received over **NDI** (DistroAV plugin).
- **Connections**: each saved host is a pair: a MiSTer IP, plus an OBS PC IP and NDI source name.

Security is out of scope (LAN only: no auth, no TLS). OBS PC setup is in [SETUP-NDI.md](SETUP-NDI.md).

Status: **planning**. Nothing is built yet.

---

## 0. Decisions so far

| # | Decision |
|---|---|
| D1 | Video/audio transport is **NDI** via the DistroAV plugin, one **NDI filter (dedicated output) per MiSTer capture source**. NDI encoding runs on the CPU (SpeedHQ), so it works on any OBS PC and doesn't depend on a specific GPU. |
| D2 | **One MiSTer live at a time.** Tabs or a host list are fine, but only the active session receives video/audio. The others are disconnected from NDI to save bandwidth. |
| D3 | **No mrext changes for now.** Gaps go in the backlog (§7). We use only the existing WS API. |
| D4 | Start with the MiSTer connected **directly over HDMI**. The RetroTINK 4K path comes later (4K60 NDI is ~250+ Mbps). |
| D5 | **Accessibility permission is required.** The app explains why on first run: to capture ⌘Tab, ⌘Space and other system shortcuts while in capture mode. |
| D6 | **Exit capture** = **Right ⌘ + Right ⌥ + Esc** (Windows: Right Win + Right Alt + Esc). **Enter capture** = click the video, and also automatically when a connection starts. |
| D7 | The OBS PC must not be hard-coded. It is just an IP plus an NDI source name per profile. |
| D8 | **Rust core (backend) + native shells.** Swift/SwiftUI for the macOS shell only; other OSes get their own native shell on the same core. |
| D9 | **Sidebar** of hosts with one main view (no tabs). |
| D10 | Minimum **macOS 14**. It may go higher if an API requires it, since hosts are modern Macs. |
| D11 | Distributed **to others**, not just personal use. NDI is used directly through the NDI SDK, **loaded dynamically** (see §2.1). |
| D12 | **Target machines are early-to-mid-90s computers**, usually **640×480 or higher** (800×600, 832×624, 1024×768, ...). Display decisions should favour pixel-accurate desktop/text readability over game-style scaling. |
| D13 | The **latency/stats readout is in the MVP** (toggleable overlay). |
| D14 | **No required MiSTer config changes.** The app must "just work" with whatever the MiSTer outputs. Ini tweaks are documented only as optional quality tips (§4.1). |
| D15 | **License: MIT.** It is compatible with Mac App Store distribution (GPL is not). See §2.2 for App Store constraints. |
| D16 | **First target core: HP9K715** on MiSTer `<mister-ip>` (HDMI-direct). |

---

## 1. What exists today

### mrext Remote (`../mrext`), used as-is

The WS endpoint is `ws://<mister>:8182/api/ws`. Commands are plain-text messages:

| Purpose | WS message | Notes |
|---|---|---|
| Key down / up | `kbdRawDown:{code}` / `kbdRawUp:{code}` | Linux uinput keycodes, no sleep. **We use these for all keyboard input.** |
| Key tap | `kbdRaw:{code}` | 40 ms sleep inside, so we avoid it |
| Mouse relative | `mouseMove:{x},{y}` | Default mouse mode |
| Mouse absolute | `mousePos:{x},{y}` | Per-mille 0–1000, core-dependent (later) |
| Mouse buttons | `mouseBtn:left_down` / `left_up` / `right_*` / `middle_*` | 3 buttons |
| Status (server→client) | `coreRunning:`, `gameRunning:`, `menuNavigation:` | Shown in the status bar |

Known limits we live with for now: no mouse wheel, no ping/latency echo, and each message is logged at Info on the MiSTer.

### OBS + DistroAV
- The **NDI filter** on a source sends that source's video and audio as its own named NDI stream at the source's native resolution, independent of the OBS canvas.
- **obs-websocket** (built in) is optional for later: recording, scene control.

---

## 2. Cross-platform architecture

Goal: the app feels native on macOS now and can be ported to Windows (and maybe Linux/iPad) later without rewriting the hard parts.

### Recommendation: shared **Rust core** + thin **native shells**

```
┌──────────────────────── Native shell (per OS) ────────────────────────┐
│ macOS: SwiftUI (windows, tabs, settings, toolbar, paste box)          │
│        AppKit NSView hosting a CAMetalLayer (video surface)           │
│        CGEventTap + NSEvent (keyboard/mouse capture, cursor lock)     │
│ Windows (later): WinUI 3 or Win32 shell, HWND surface, Raw Input /    │
│        low-level keyboard hook                                        │
└──────────────┬───────────────────────────────────────▲────────────────┘
   commands    │  (C ABI via UniFFI: Swift/C#/Kotlin)   │ events
   + raw input │                                        │ (status, stats)
┌──────────────▼───────────────────────────────────────┴────────────────┐
│ Rust core ("engine"), the same code on every platform                │
│  • Session manager: profiles, connect/disconnect, active session      │
│  • mrext client: WebSocket, HID→uinput keymap, held-key tracking,     │
│    stuck-key release, mouse coalescing (~250 Hz), capture chord       │
│  • Paste-as-typing: text → key sequences with configurable delay      │
│  • NDI receiver: NDI SDK (C) via bindgen                              │
│  • Renderer: wgpu (Metal on Mac, D3D12/Vulkan elsewhere) draws into   │
│    the shell's native surface: zoom/pan, integer scale, nearest/linear│
│  • Audio: cpal (CoreAudio / WASAPI)                                   │
└───────────────────────────────────────────────────────────────────────┘
```

Why this split:
- **Latency-critical work stays in one process.** Video frames go from the NDI SDK to the GPU texture to the screen with no IPC hop. A separate client/server *process* would add a copy and latency for no benefit, since everything runs on the same machine.
- **We still get the "client/server" benefit internally.** The shell talks to the core only through commands and events, so the core could later run headless or behind a socket (e.g. a CLI, or an iPad client) without redesign.
- **Platform-neutral input.** Shells translate OS keycodes into **USB HID usage codes**, which distinguish left and right modifiers. The core maps HID → Linux uinput and owns all logic (chord detection, held keys, paste typing). A Windows port only needs a new OS→HID table plus the hook code.
- **Rendering and audio are written once** (wgpu, cpal), so zoom, integer scaling and future CRT shaders behave the same everywhere.
- **Native UI per OS**, so the Mac app looks and behaves like a Mac app.

### 2.1 NDI integration and licensing

- We use the **NDI SDK** (free, proprietary) directly from the Rust core. NDI's C headers are MIT-licensed and may live in our repo. We generate Rust bindings from them with bindgen.
- **Dynamic loading** (NDI's preferred approach for open-source projects): the core loads `libndi` at runtime instead of linking it at build time.
  - Search order: the copy bundled inside the app → `NDI_RUNTIME_DIR_V6` → the standard install location.
  - If it isn't found, the app shows a clear message with a link to <https://ndi.video/tools/>.
- **Requirements for distributing to others** (from NDI's [Software Distribution](https://docs.ndi.video/docs/sdk/software-distribution) / [Licensing](https://docs.ndi.video/docs/sdk/licensing) pages):
  - Show a link to **ndi.video** near where NDI is used or selected in the UI (the source field and the About box), on the project website and in the docs.
  - Ship NDI's library notice file with any bundled NDI binaries.
  - Bundle NDI libraries **inside the app**, never in system paths.
  - Don't put "NDI" in the product name without following the brand guidelines. *MiSTerKVMS* is fine.
- **Our own license**: MIT (D15). mrext is GPL, but we only talk to it over the network, which is fine.
- Read the full NDI SDK license agreement before the first public release.

### 2.2 Mac App Store vs direct download

The App Store requires the **App Sandbox**, and that affects three things:

| Concern | Direct download (notarized DMG) | Mac App Store (sandboxed) |
|---|---|---|
| Capture ⌘Tab / ⌘Space etc. | ✅ Active CGEventTap with Accessibility permission | ⚠️ **Likely not allowed.** Sandboxed apps generally can't get Accessibility trust. Input Monitoring (listen-only) can't *block* ⌘Tab. **To verify in Spike 1.** |
| NDI library | Load from the app bundle or the installed runtime | Must be **bundled inside the app** (no loading from outside the sandbox) |
| Network (WS + NDI/mDNS) | ✅ | ✅ with `network.client` / `network.server` entitlements, plus a Local Network usage description |

Plan: build both from the same code. The direct-download build has full capture. If the App Store build can't capture system shortcuts, it falls back to: regular keys and most ⌘ combos captured while focused, plus toolbar buttons to send ⌘Tab-style combos. Decide once Spike 1 shows what the sandbox allows.

Alternatives considered:
- **All-Swift**: fastest to start on Mac, but nothing carries over to Windows except the design.
- **Qt 6 (C++)**: one codebase (it's what OBS uses), but not quite native-looking, and we'd still need per-OS input hooks.
- **Tauri/Electron/Flutter**: getting uncompressed NDI frames into a web/Flutter view adds copies and latency. Not a good fit for video.

---

## 3. Input design

- **Capture mode**
  - It starts automatically when a session connects, and again when the user clicks the video.
  - On entering, show a brief toast: "Press **Right ⌘ + Right ⌥ + Esc** to release".
  - While captured, every key goes to MiSTer, including ⌘-combos and ⌘Tab (via CGEventTap). The cursor is hidden and locked, and raw mouse deltas are sent as `mouseMove`.
- **Exit chord: Right ⌘ + Right ⌥ + Esc**
  - When Esc is pressed while both right modifiers are held, the Esc is **not** forwarded. The app releases every key/button held on MiSTer, then leaves capture.
  - The two modifiers will already have been sent to MiSTer as key-downs; the release step sends their key-ups.
  - Windows note: Right Alt is AltGr on many non-US layouts, so we may need an alternative chord there.
- **Stuck-key safety**: send key-up/button-up for everything held on capture exit, window blur, tab switch and disconnect.
- **Drop macOS key auto-repeat**, since MiSTer/Linux generates its own.
- **Middle mouse button**: HP-UX/CDE and other Unix workstations use 3 buttons (middle = paste). mrext supports middle down/up. Mac trackpads and mice often have no middle button, so add an emulation option: **⌥-click → middle** (configurable), and optionally "left+right together → middle".
- **Keymap**: US layout first. Per-profile overrides come later for Amiga/Atari/C64-style keys.

---

## 4. Video / display

**Where scaling happens.** There are two stages, and the first one matters most:

1. **MiSTer → capture card (HDMI)**: MiSTer's own scaler turns the core's resolution into the HDMI output mode. **Any blur or lost pixels from this step can't be undone later.**
2. **Our app (display)**: takes the received frame and fits it to the window. OBS isn't involved in scaling, because the NDI filter sends the source at its native resolution.

### 4.1 What the MiSTers output today (checked 2026-10-10)

**Primary: `<mister-ip>`** (HDMI-direct, first target)
- Core: **HP9K715**. Its `[HP9K715]` ini section sets `video_mode=8`, so the output is **1920×1080@60** (confirmed via `/api/controls/screen`).
- The core scales by whole steps itself (OSD "Scale", default V-Integer). The HP's **1280×1024** (or 1024×768) desktop shows **1:1** inside 1080p with black borders. **This is the ideal case.** The app auto-crops to 1280×1024 and displays it pixel-exact.
- The global ini uses `main=ConsoleMode/MiSTer_ConsoleMode` (a custom Main) and `video_mode=0`.

**Other: `<second-mister-ip>`**
- Core: **MacPPC7300**. No core section, so it uses the default **1280×720**, `vscale_mode=0`.

Per D14, the app must work with any of these as-is. Auto-crop + good scaling makes 720p output usable. These tables only explain *why* an optional tweak looks better.

What 720p fit-to-height does to 90s desktop resolutions:

| Guest resolution | At 720p, `vscale_mode=0` | At 1080p, `vscale_mode=1` (integer) |
|---|---|---|
| 640×480 | 1.5× (uneven pixels, soft) | **2×** = 1280×960, sharp |
| 800×600 | 1.2× (uneven) | **1×** = 800×600, sharp, letterboxed |
| 832×624 | 1.15× (uneven) | **1×**, sharp, letterboxed |
| 1024×768 | **0.94×, downscaled, so pixels are lost** (text is mushy) | **1×**, sharp, letterboxed |

**Optional quality tip (documented in the app's help, never required):** for computer cores, a per-core section with 1080p (`video_mode=8`) and integer scaling (`vscale_mode=1`, or the core's own integer scale option) makes MiSTer send a pixel-exact picture. The app then crops and scales it with nearest-neighbor, so text stays crisp. The `.92` MiSTer already does this for HP9K715, SGIIndy and SunSparcStation.

### 4.2 App display features
- **Auto-crop to active area.** Detect the black letterbox/pillarbox borders and show only the guest picture.
  - The real guest resolution isn't available from mrext, so we infer it from the frame.
  - The status bar shows the detected size (e.g. "800×600 ×1").
  - Can be switched off, or set to a manual crop per profile.
- **Fit** (aspect-correct), **1:1**, and **integer scale** of the cropped picture.
- **Nearest-neighbor vs smooth** filtering. Nearest is the default for computer cores.
- **Zoom in/out + pan**: ⌘+ / ⌘− and pinch while not captured, plus toolbar buttons.
- **Mouse in zoomed view**: relative mouse mode isn't affected by zoom. Absolute mode (later) would have to map through crop and zoom.

### 4.3 Latency / stats overlay (MVP, D13)
A small toggleable overlay in a corner of the video:
- **Video**: received fps, dropped/late frames, stream resolution and detected guest resolution, and **frame age**: the NDI frame timestamp compared with when we put it on screen. Frame age needs the OBS PC and Mac clocks in sync (NTP, normally within a few ms). It is labelled as an estimate.
- **Input**: WebSocket connection state, messages per second, and queued messages. True key-to-pixel round-trip time can't be measured until mrext has a ping (backlog). Until then, the overlay includes a **manual test**: press a button, the app sends a keystroke and timestamps when the picture changes in a chosen region. That gives a real end-to-end number without changing mrext.

---

## 5. Features

### MVP (v0.1)
1. Host profiles (name, MiSTer IP, OBS PC IP, NDI source name, key delay for pasting).
2. One window with a **sidebar of hosts**. Selecting one connects it; only the active one is live (D2, D9).
3. NDI video + audio from OBS.
4. Full keyboard + relative mouse via the mrext WS, with auto-capture, the exit chord and stuck-key safety.
5. Auto-crop, zoom/pan, fit / 1:1 / integer scale, nearest/smooth (§4.2).
5b. Latency/stats overlay (§4.3).
6. **Toolbar**: buttons for **MiSTer menu (F12)**, plus a visible hint that F12 is the MiSTer menu key. Also connect/disconnect, zoom controls and "Type text…".
7. **Type text (paste-as-typing)**
   - Opened from the toolbar or from an overlay shown when capture is released. You need a text box because typing in capture mode goes to MiSTer.
   - A paste/edit box takes **plain text only** (printable ASCII + Enter/Tab at first). Characters that can't be mapped are listed before sending.
   - **Delay between keys** and **key hold time** are configurable per profile in Options, because different cores accept keys at different speeds.
   - Has Start / Stop and a progress bar.
8. Status bar: connected state, current core/game (mrext events), resolution and fps of the stream.
9. Auto-reconnect.

### Later
- **Recording** of a session: on the OBS PC via obs-websocket, or locally on the Mac.
- Quick actions: reset, savestates 1–4, MiSTer screenshot (`POST /api/screenshots`), local PNG screenshot.
- Grid / multi-view of all MiSTers, and picture-in-picture.
- CRT/scanline shader.
- Per-profile keymap overrides and absolute mouse mode.
- RetroTINK 4K path (bandwidth testing, maybe NDI|HX).
- MiSTer auto-discovery.
- Windows shell, then possibly Linux/iPad.
- Gamepad passthrough (parked; needs mrext support).

---

## 6. Open questions

Resolved: Q-A → D8, Q-B → D9, Q-C → D10, Q-D → D11, Q-E → §4.1.

Resolved: Q-F → D14 (no required ini changes), Q-G → D15 (MIT), Q-H → D16 (HP9K715).

- **Q-I. App Store build:** if the sandbox blocks system-shortcut capture (§2.2), is a reduced App Store build acceptable, with the full version as a direct download?

---

## 7. Backlog: mrext (not now, per D3)

- Expose mouse wheel over the WS (`Mouse.Wheel` exists in `pkg/input/mouse.go`).
- Lower the per-message WS log level (`cmd/remote/websocket/websocket.go`).
- Add a `ping` echo command for input latency measurement.
- Optional message batching.
- Virtual gamepad device.

---

## 7b. Shelved ideas

- **Video straight from the MiSTer (no capture card/OBS).** The HDMI scaler keeps each frame at the core's native resolution in DDR (`0x20000000`, header with width/height; Main_MiSTer's `scaler.cpp` reads it for screenshots). A MiSTer-side service could stream it: native pixels, ~30–60 ms instead of ~180 ms, true frame rate.
  - **Shelved (2026-10-11) as too risky for now.** The DE10-Nano's 2×800 MHz ARM can't do H.264, so it would need delta + fast compression (fine for desktops, drops on full-screen motion). It also adds CPU and DDR load that some cores need, only works with the scaler (not `direct_video`), and needs a service installed on the MiSTer.
  - **No audio either way:** sound never reaches Linux on the MiSTer, so audio would still need the capture path.

## 8. Progress (2026-10-10)

- **Spike 1 (input): built.** Rust core (`core/`) with mrext WS client, HID→Linux keymap, exit chord, stuck-key release, alt-click→middle, 250 Hz mouse coalescing. Swift shell (`apple/`) with host sidebar, CGEventTap capture (in-app fallback without Accessibility), F12 toolbar button.
- **Spike 2 (video): built.** NDI runtime loaded dynamically (`core/src/ndi`, headers vendored in `third_party/ndi`), wgpu renderer drawing into the NSView's Metal layer (fit / integer / 1:1, nearest/smooth, zoom/pan, auto-crop), cpal audio with a ~40 ms buffer, stats overlay.
- CLI check against `OBSPC (MiSTer-1)`: 1920×1080 BGRA at 59.9 fps, 0 dropped, 48 kHz stereo audio.
- Found: the OBS PC's clock is ~2.1 s off the Mac's, so NDI "frame age" can't be trusted until Windows time sync is fixed. The overlay says so instead of showing a bogus number.

### Since then
- **Predictive cursor** (green while ahead, black when synced, amber when lost), with latency/gain learned over a rolling minute and saved per host. **Raw mouse input** option (no double acceleration).
- **Recording** to MP4 (H.264 + AAC) through the core's `MediaSink` hook and AVFoundation.
- **`kvms` CLI** for core development: status, screenshots (capture or MiSTer-native), recording, frames, typing, key combos, mouse, wait-for-change, load-core. Ships inside the app.
- App **icon** (MiSTer-Kun on a retro computer) and **About** window with credits.
- **Release pipeline:** universal DMG on every push to `main`, optional Developer ID signing and notarization.
- **Port plans:** [PORTING.md](PORTING.md), [PORT-WINDOWS.md](PORT-WINDOWS.md), [PORT-LINUX.md](PORT-LINUX.md).

## 9. Next steps

1. **OBS PC**: install DistroAV and add an NDI filter to the HDMI-direct MiSTer source ([SETUP-NDI.md](SETUP-NDI.md)). Verify on the Mac with NDI Video Monitor.
2. Answer Q-I.
3. **Spike 1 (input)**: Rust core with mrext WS + HID keymap, a minimal Swift shell with CGEventTap capture and the exit chord. Test on the MiSTer menu, the OSD (F12) and **HP9K715** on `<mister-ip>` (HP-UX/CDE: typing, 3-button mouse). Also test whether a sandboxed build can still block ⌘Tab (§2.2).
4. **Spike 2 (video)**: NDI receive → wgpu render into a CAMetalLayer + cpal audio. Measure glass-to-glass latency by filming a stopwatch shown on the MiSTer and on the Mac.
5. Build the MVP on top of both.
