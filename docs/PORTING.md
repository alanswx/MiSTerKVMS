# Porting MiSTerKVMS: shared groundwork

> **Note:** NDI was removed in 1.0; video now comes from MiSTerKVMS Capture streams or a local capture card — see [CAPTURE-SERVER.md](CAPTURE-SERVER.md).

The Windows and Linux plans ([PORT-WINDOWS.md](PORT-WINDOWS.md), [PORT-LINUX.md](PORT-LINUX.md)) build on this document. It covers what is already portable, what's tied to macOS, and the groundwork both ports need first.

> **Update (October 2026):** §2 items 1–4 and 7 are done: profiles and the `Controller` live in core and both shells use them, the renderer takes a `NativeSurface`, the ffmpeg recorder is the fallback `MediaSink`, and CI builds macOS and Windows. Paste-as-keystrokes in the GUI (item 5) is still to do. The table below is the v0.9 starting point.

## 1. Where things stand (v0.9)

About 4,000 lines of Rust are shared. About 1,100 lines of Swift are the macOS shell.

| Area | Where | Portable? |
|---|---|---|
| mrext WebSocket session, reconnect, mouse coalescing | `core/src/session.rs` | ✅ Pure Rust |
| Input state machine: held keys, exit chord, Alt-click→middle, release-all | `core/src/input.rs` | ✅ |
| HID→Linux keymap, text/combos → HID | `core/src/keymap.rs`, `core/src/text.rs` | ✅ |
| NDI loading and receiving | `core/src/ndi` | ✅ Library names/paths already per-OS (`Processing.NDI.Lib.x64.dll`, `libndi.so.6`) |
| Video receive, auto-crop, predictive cursor, stats | `core/src/video/{mod,crop,cursor}.rs` | ✅ |
| GPU renderer (wgpu) | `core/src/video/render.rs` | ⚠️ Rendering is portable. **Surface creation only knows AppKit** (`create_surface`). |
| Audio out (cpal) | `core/src/video/audio.rs` | ✅ (WASAPI / ALSA / PipeWire through cpal) |
| Recording hook | `MediaSink` trait in `core/src/video/mod.rs` | ✅ Trait. The encoder behind it is per-OS. |
| CLI (`misterkvms-cli`) | `cli/` | ✅ Builds anywhere the core does. Needs `ssh`/`scp`, and `ffmpeg` for MP4. |
| Window, sidebar, toolbar, settings, About | `apple/Sources/MiSTerKVMS/Views` | ❌ SwiftUI |
| Keyboard/mouse capture, OS keycode→HID, raw mouse | `apple/.../Input` | ❌ CGEventTap, IOHID |
| Host profiles + persistence | `apple/.../Model/HostProfile.swift` | ❌ UserDefaults (should move to core, see §2) |
| Session wiring: stats timer, overlay flashing, model saving, display persistence | `apple/.../Model/SessionController.swift` | ❌ Mostly logic that belongs in core (§2) |
| Recorder (H.264/AAC) | `apple/.../Model/Recorder.swift` | ❌ AVFoundation |
| Build, sign, package | `scripts/*.sh`, `.github/workflows/release.yml` | ❌ macOS only |

## 2. Groundwork before either port (do once, in core)

These make each new shell thin and keep the platforms behaving the same. The macOS shell is then switched over to use them.

1. **Profiles and app settings in core.** Add `core/src/profiles.rs`:
   - A `HostProfile` record with the same fields as today: hosts, NDI source, mouse options, display options, cursor model.
   - JSON load/save in the platform config directory (`dirs` crate: `~/Library/Application Support/MiSTerKVMS`, `%APPDATA%\MiSTerKVMS`, `~/.config/misterkvms`).
   - One-time migration of the macOS UserDefaults data.
   - A **portable export/import** of profiles is then free.
2. **A `Controller` object in core** that owns the `MisterSession` + `VideoSession` pair and the logic that's in `SessionController.swift` today:
   - stats polling and the "show for 5 s on change, pin" overlay rule
   - saving the cursor model every minute
   - auto-capture on connect
   - display-settings persistence
   - recording state

   Shells then render state and forward events. The UniFFI surface becomes one object with an observer, which keeps the bindings small for C#.
3. **Renderer surface creation for every window system.** Turn `render::create_surface(ns_view)` into `create_surface(handle: NativeSurface)` with variants:
   - `AppKit { ns_view }`
   - `Win32 { hwnd, hinstance }`
   - `Wayland { display, surface }`
   - `Xlib { display, window }`
   - `Xcb { connection, window }`

   wgpu supports all of these through `raw-window-handle`. Linux may instead need a texture-export path; see the Linux plan.
4. **Cross-platform recorder in core** as a fallback `MediaSink`, so a new shell gets recording without writing an encoder:
   - Option A: spawn `ffmpeg` with a raw pipe. The CLI already does this, and it's the simplest.
   - Option B: per-OS encoders, Media Foundation on Windows and GStreamer on Linux.

   The plans below say which each port uses.
5. **Paste-as-keystrokes in core.** Typing with per-host delay and hold, start/stop and progress. `text.rs` already has the US mapping; this adds the timed sender. It's on the MVP list and every shell needs it.
6. **Keymap tests per OS.** A table-driven test per shell keymap (OS code → HID → Linux code) so a typo can't silently send the wrong key.
7. **CI matrix.** Extend `.github/workflows/release.yml`:
   - `test` job: core and CLI tests on macOS, Windows and Ubuntu.
   - `build-<os>` jobs, each uploading a release asset.
   - The release step collects all assets.
   - The version scheme stays the same (MAJOR.MINOR + auto patch).

Rough size: 1–2 weeks of focused work. Items 1–3 should come before either port starts.

## 3. Shell responsibilities on any platform

A port is done when its shell covers the following, built on the core APIs:

- **Window:** a host sidebar, the video surface and a toolbar. The toolbar has MiSTer menu (F12), capture/release, scale (Fit/Integer/1:1), smooth, auto-crop, predictive cursor, zoom, record and mute.
- **Status bar:** connection, current core, recording timer, last recording.
- **Stats overlay:** shows for 5 s on changes, can be pinned, and has a keyboard shortcut.
- **Host editor:** every profile field, plus the raw-mouse option and its permission note.
- **Capture mode:**
  - Grab all keys, including system shortcuts where the OS allows it.
  - Lock and hide the pointer and send raw relative deltas.
  - Exit chord Right-Meta + Right-Alt + Esc, configurable.
  - Release everything on focus loss.
- **OS keycode → HID** mapping, left/right modifiers distinguished.
- **Recording**, through core's fallback or a native encoder.
- **About window** with the same credits: MiSTer-Kun (HeWhoisRed, baxysquare), MiSTer, mrext.
- **Packaging, signing and CI release** for the platform. Ship the `misterkvms-cli` CLI too.

## 4. Order of work

1. Groundwork (§2).
2. **Linux and Windows CLI first.** `misterkvms-cli` already compiles there, so it's useful on day one for core developers working on those machines. It needs only CI packaging.
3. Windows shell ([PORT-WINDOWS.md](PORT-WINDOWS.md)). Most MiSTer users who'd run this are likely on Windows, and the OBS PC is often the same machine.
4. Linux shell ([PORT-LINUX.md](PORT-LINUX.md)).
