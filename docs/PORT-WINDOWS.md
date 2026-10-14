# Windows port plan

Prerequisite: the shared groundwork in [PORTING.md](PORTING.md) §2 (profiles and controller in core, multi-platform surface creation, fallback recorder).

Target: Windows 10 22H2 and Windows 11, x64 first, ARM64 second.

## Status (October 2026)

W0–W5 and W7 (unsigned) are done; the app is in `windows/`, built by `scripts/build-windows.ps1` and CI. Where it differs from the plan below:

- **Bindings:** a small hand-written C interface (`core/src/capi.rs`: create/free the controller, one JSON request/reply call, and an event callback) instead of uniffi-bindgen-cs, which lags our UniFFI version. Almost everything goes through the core `Controller`, so the surface stays tiny.
- **Video surface:** neither a child HWND nor a SwapChainPanel. The core creates a DirectComposition surface handle; wgpu builds its swap chain on it (`NativeSurface::CompositionHandle`), and the shell wraps the handle with `ICompositorSwapChainInterop::CreateCompositionSurfaceForHandle` in a brush on a XAML visual. No airspace problem, so the stats overlay, capture hint and dialogs stay in XAML; no UI-thread swap-chain calls, and DPI is handled by sizing the swap chain in pixels.
- **Input capture lives in the core** (`core/src/win/capture.rs`), on its own thread: low-level keyboard + mouse hooks (both swallowed), Raw Input for raw mode, the pointer parked and hidden under a 1-pixel window of ours, AltGr's fake Left Ctrl dropped (scan code 0x21D or same timestamp), and it ends on the chord, focus loss or disconnect. Accelerated movement is divided by the display scale, so it's in points like on macOS.
- **Recording (W6):** the core's ffmpeg recorder (`core/src/video/ffmpeg.rs`) rather than Media Foundation; needs ffmpeg on the PATH or next to the app.
- **Packaging:** a self-contained zip (WinUI component packages only, ~70 MB). MSIX, signing and winget are still to do; so is ARM64 (check NDI's ARM64 runtime first).

Still to verify by hand: an AltGr layout, Raw Input on a high-DPI mouse, mixed-DPI monitors, an 8-hour soak (§8).

## 1. Architecture

```
┌──────────── Windows shell (C#, WinUI 3 / Windows App SDK) ────────────┐
│ MainWindow: NavigationView sidebar, CommandBar toolbar, status bar     │
│ VideoHost: child HWND (or SwapChainPanel) the core renders into        │
│ InputCapture: Raw Input + low-level keyboard hook + ClipCursor          │
│ About / host editor / settings pages                                   │
└───────────────▲───────────────────────────────┬────────────────────────┘
                │ UniFFI C# bindings             │ events (observer)
┌───────────────┴───────────────────────────────▼────────────────────────┐
│ kvms_core.dll (Rust): Controller, MisterSession, VideoSession,         │
│ wgpu (D3D12, Vulkan fallback), cpal (WASAPI), NDI (Processing.NDI.Lib) │
└────────────────────────────────────────────────────────────────────────┘
```

### Shell technology: recommendation and alternatives

| Option | Native feel | Effort | Risk |
|---|---|---|---|
| **WinUI 3 (C#) + uniffi-bindgen-cs** (recommended) | Best: Windows 11 look, Mica, proper accessibility | Medium | `uniffi-bindgen-cs` is a community generator and has to track our UniFFI version (0.32). Mitigation: pin both, and keep the exported surface small (the controller from PORTING §2). |
| WPF (C#) + same bindings | Good, older look | Medium | Same bindings risk; mature and simple hosting of a child HWND. |
| Rust-only Win32 shell (`windows` crate) + a Rust UI kit (Slint / egui) | Fair | Medium | One language, and could share the shell with Linux. Not truly native-looking. |

Fallback if C# bindings are a problem: export a small C ABI from core by hand (≈30 functions) and P/Invoke it. The controller design keeps that feasible.

## 2. Video surface

- wgpu creates a D3D12 surface from a **Win32 HWND** (`RawWindowHandle::Win32 { hwnd, hinstance }`). WinUI 3 content is DirectComposition, not HWND-per-control, so:
  - **Plan A:** a child HWND sized to the video area, created by the shell and kept in sync on resize/DPI change. This is simple and proven. Overlays (stats, capture hint) must then be separate top-level popups or drawn by the core renderer, because XAML can't draw over a child HWND ("airspace").
  - **Plan B:** a `SwapChainPanel` with a DXGI swap chain made by wgpu's DX12 backend (`SurfaceTargetUnsafe::SwapChainPanel` / composition visual). This lets XAML overlays draw on top. Spike it first; fall back to A.
- Move the **stats overlay and capture hint into the core renderer** (text via a small glyph atlas). That avoids airspace problems on Windows and Linux, and the macOS shell can keep SwiftUI overlays.
- Present mode: Fifo with `desired_maximum_frame_latency = 1`. Test `Mailbox` on D3D12 for lower latency with VRR displays.
- DPI: pass physical pixels and the scale factor (`GetDpiForWindow / 96`) to `resize`. Handle `WM_DPICHANGED`.

## 3. Input capture

| Need | Windows mechanism |
|---|---|
| All keys while captured, including Alt+Tab, Win, Win+R, Ctrl+Esc, Alt+F4 | **Low-level keyboard hook** `SetWindowsHookEx(WH_KEYBOARD_LL)` installed only while captured. It returns 1 to swallow. No admin rights needed. |
| Keys that can't be intercepted | **Ctrl+Alt+Del** (Secure Attention Sequence) and **Win+L**. Offer a toolbar menu "Send Ctrl+Alt+Del" (the CLI already has `key ctrl+alt+del`). |
| Key identity | Hook gives `scanCode` + `LLKHF_EXTENDED`. Map **PS/2 set-1 scan codes (+E0 extended) → HID** in a table. Don't use virtual-key codes, which depend on the layout. |
| Left vs right modifiers | Scan codes distinguish them (RCtrl/RAlt = E0 1D / E0 38, RWin = E0 5C). |
| **AltGr quirk** | On non-US layouts Right Alt generates a fake LCtrl down first (scan 1D with an injected flag or the same timestamp). Detect and drop the fake LCtrl. The exit chord RWin+RAlt+Esc then works. Also offer a configurable chord (e.g. Ctrl+Alt+Shift+F12). |
| Auto-repeat | Hook sees repeats as repeated key-downs. Drop them (core already ignores repeated downs). |
| Raw mouse (unaccelerated) | **Raw Input**: `RegisterRawInputDevices` for mouse (usage page 1, usage 2), reading `WM_INPUT` → `RAWMOUSE.lLastX/lLastY` relative deltas. This is native raw input, so "raw mouse" needs no permission. |
| Accelerated mouse (non-raw mode) | `WM_MOUSEMOVE` deltas while the cursor is re-centred, or apply the Windows pointer-speed curve. Simplest: always use Raw Input and offer a "match Windows pointer speed" multiplier. |
| Pointer lock + hide | `ClipCursor` to the video rect, `ShowCursor(FALSE)`. Release on capture end, `WM_ACTIVATEAPP` false, `WM_KILLFOCUS`. |
| Wheel | Raw Input wheel data. mrext has no wheel command yet (backlog), so it's ignored for now. |
| Buttons | Raw Input button flags (left/right/middle; X1/X2 ignored). |
| Focus loss safety | Release-all on deactivate (same core call as macOS). |

UIPI note: the hook doesn't see input when an elevated window has focus. That's irrelevant while our window is focused.

## 4. Audio, NDI, recording

- **Audio:** cpal WASAPI shared mode works as is. Test latency (WASAPI shared adds ~10–20 ms). Consider exclusive/low-latency mode later.
- **NDI:** users install the **NDI Runtime** (or NDI Tools). Core finds `Processing.NDI.Lib.x64.dll` via `NDI_RUNTIME_DIR_V6`. NDI licensing allows bundling the runtime DLL in our installer: the NDI redistributable, kept up to date, with the license text. Decide at packaging time; bundling is friendlier.
  - ARM64: check that NDI 6 ships a Windows ARM64 runtime. If not, the ARM64 build is CLI/input-only, or runs as x64 under emulation.
  - Firewall: the first NDI receive prompts Windows Firewall for private networks. Document it.
- **Recording:** Media Foundation `IMFSinkWriter` (H.264 + AAC, hardware MFTs on NVIDIA/AMD/Intel), implemented in Rust (`windows` crate) behind `MediaSink`. Inputs are BGRA → NV12, converted by the MF video processor MFT, or by a compute pass in wgpu before readback. Fallback: core's ffmpeg sink (PORTING §2.4).

## 5. Other features

- **Settings location:** `%APPDATA%\MiSTerKVMS\profiles.json`, through core.
- **About:** WinUI `ContentDialog` or page with the same credits.
- **App icon:** `make_icon.py` also writes an `.ico` (16, 24, 32, 48, 64, 256), using Pillow or `magick`.
- **Theme:** follow the system light/dark theme. Mica backdrop.
- **CLI:** `misterkvms-cli.exe` built by the same CI. Uses Windows' built-in OpenSSH `ssh`/`scp` for `load-core`. `ffmpeg` is optional, for MP4.

## 6. Packaging, signing, distribution

- **Installer:** MSIX for clean install/uninstall and Store eligibility, plus a portable zip. MSIX apps run full-trust, so low-level hooks and Raw Input work. Check Store policy on input hooks before submitting. Alternative: an Inno Setup/MSI installer that also installs the NDI runtime.
- **Code signing:** Azure Trusted Signing (cheapest standard option) or an OV/EV certificate. Without signing, SmartScreen warns.
- **winget:** submit a manifest pointing at release assets.
- **CI** (`windows-latest`):
  1. `cargo test`
  2. Build core for x64 and ARM64: `x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc`.
  3. Generate C# bindings.
  4. `dotnet publish` for each RID.
  5. MSIX + zip, signed if the secrets are present.
  6. Upload to the same GitHub release as the macOS DMG.

## 7. Milestones

| # | Milestone | Done when |
|---|---|---|
| W0 | Groundwork (PORTING §2) merged | macOS app runs on the core controller and profiles |
| W1 | `misterkvms-cli.exe` in releases | CLI works against a MiSTer from Windows |
| W2 | Bindings spike | C# console app drives `MisterSession` through uniffi-bindgen-cs (or the C ABI fallback) |
| W3 | Video spike | Child HWND (or SwapChainPanel) shows the NDI stream through wgpu D3D12, with resize/DPI |
| W4 | Input spike | Hook + Raw Input + ClipCursor capture; exit chord incl. AltGr handling; keys verified on HP9K715 and a DOS core |
| W5 | Shell MVP | Sidebar, toolbar, status bar, host editor, overlays, auto-capture, zoom/scale/crop, predictive cursor |
| W6 | Recording | Media Foundation sink (or ffmpeg fallback) |
| W7 | Packaging | Signed MSIX/zip in CI releases; About, icon |

Estimate: W2–W7 is about 3–5 weeks for one developer, most of the risk in W2/W3.

## 8. Test plan

- Keyboards: US and a non-US AltGr layout (German or UK). Verify every HID key on a core with a key tester (ao486 + a DOS keyboard test tool).
- Exit chord with AltGr; Ctrl+Alt+Del button; Alt+Tab and Win key swallowed while captured.
- Raw mouse on a 1600-DPI mouse vs a trackpad (Precision Touchpad also reports through Raw Input).
- Multi-monitor with mixed DPI; window moved between monitors while rendering.
- NDI on the **same PC as OBS** (loopback): a common setup on Windows. Check latency and CPU.
- 8-hour soak: memory stable, no audio drift (buffer stays bounded).
