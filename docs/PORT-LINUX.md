# Linux port plan

Prerequisite: the shared groundwork in [PORTING.md](PORTING.md) §2.

Targets: current GNOME and KDE on **Wayland** (primary), X11 sessions (secondary), x86_64 and aarch64 (e.g. Raspberry Pi 5). Distribution through **Flatpak** first.

## Status (October 2026)

The shell is in `linux/` (GTK 4.14 + libadwaita 1.5, so Ubuntu 24.04 and newer), built by `scripts/build-linux.sh` and CI into a tarball with an install script. Done as planned: L1 (CLI), L2 option 1 (`NativeSurface::Offscreen`: the core renders at the widget's pixel size and reads back; the widget draws it 1:1), L3 (shortcut inhibition + pointer-constraints + relative-pointer on GDK's Wayland connection), L4 in a simpler form (X11: warp-to-centre, desktop acceleration; no XI2 raw motion yet), L5, L6 via the core's ffmpeg recorder instead of GStreamer. Still to do: Flatpak/AppImage and aarch64 builds, XI2 raw motion, the dmabuf path (L8) if the readback shows up in measurements.

## 1. Architecture

```
┌──────────── Linux shell (Rust, GTK 4 + libadwaita via gtk4-rs) ───────┐
│ AdwNavigationSplitView sidebar, header bar toolbar, status bar         │
│ VideoView: custom GtkWidget showing frames from the core (see §2)      │
│ InputCapture: shortcuts inhibit + pointer lock/relative pointer        │
│   (Wayland) or XI2 grabs + raw motion (X11)                           │
│ About (AdwAboutWindow), host editor (AdwPreferencesWindow)             │
└───────────────▲───────────────────────────────┬────────────────────────┘
                │ direct Rust calls (no FFI)    │ glib channel for events
┌───────────────┴───────────────────────────────▼────────────────────────┐
│ kvms-core: Controller, sessions, wgpu (Vulkan/GL), cpal, NDI (libndi)  │
└────────────────────────────────────────────────────────────────────────┘
```

**Why GTK 4 in Rust:** it looks native on GNOME and is acceptable on KDE. It calls the core directly (same language, no bindings). libadwaita gives the sidebar/split-view layout that matches the macOS app.

Alternative: Qt 6 via `cxx-qt`, which is better on KDE but adds C++/QML.

## 2. Video: the main risk

GTK 4 doesn't give a widget its own native window, so wgpu can't simply render into "the widget's surface" like on macOS/Windows. Options, in order of plan:

1. **MVP: upload frames as textures.** The core renders the final picture into an offscreen wgpu texture (crop/scale/zoom/predictive cursor/overlays, all as today) and reads it back. The widget wraps it in a `gdk::MemoryTexture` and draws it in `snapshot()`.
   - Simple and works everywhere.
   - Costs one GPU→CPU→GPU copy (≈8 MB per 1080p frame; fine at 60 fps on desktop hardware, borderline on a Pi).
   - Alternative: skip wgpu and let GTK scale the cropped source frame with `GtkSnapshot::append_scaled_texture(.., GSK_SCALING_FILTER_NEAREST)`, GTK 4.10+. That's even simpler, and nearest-neighbour stays sharp.
2. **Zero-copy: dmabuf.** wgpu (Vulkan) renders into an exportable image. The fd goes into `gdk::DmabufTextureBuilder` (GTK 4.14+), drawn in a `GtkGraphicsOffload` (4.14+) so the compositor can scan it out directly. Lowest latency on Wayland. It needs Vulkan external-memory export from wgpu-hal, plus modifier negotiation. Do it after the MVP works.
3. **GL path:** `GtkGLArea` with a wgpu GLES backend created from GTK's GL context (`wgpu-hal` external GL context). Fewer copies than (1), less fragile than (2). Fallback if dmabuf export is painful.

Plan: ship (1), then measure. Do (2) if the copy shows up in latency or CPU, especially on aarch64.

## 3. Input capture

| Need | Wayland | X11 |
|---|---|---|
| All keys incl. Super, Alt+Tab | `gdk::Toplevel::inhibit_system_shortcuts()`, i.e. the keyboard-shortcuts-inhibit protocol. GNOME asks the user once. KDE allows it. Some compositors refuse; then we behave like the macOS app without Accessibility, and say so. | `XGrabKeyboard` on capture (GDK seat grab). |
| Key identity | GDK hardware keycode = **evdev code + 8** on both. Map evdev → HID: the inverse of `keymap::hid_to_linux`. Nearly free, and evdev codes are what mrext wants in the end. | same |
| Left/right modifiers | evdev codes distinguish them | same |
| Pointer lock | **pointer-constraints-unstable-v1** `lock_pointer` on our `wl_surface` | `XGrabPointer` + warp to centre |
| Raw (unaccelerated) deltas | **relative-pointer-unstable-v1** `relative_motion` has both accelerated and **unaccelerated** deltas, so "raw mouse" needs no permission. | XInput2 `XI_RawMotion` (raw values) |
| Hide pointer | `gtk::Widget::set_cursor(None)` / blank cursor | same |
| Focus loss safety | `notify::is-active` on the window → release-all | same |

GTK 4 has no public API for pointer lock / relative pointer, so the shell talks to Wayland directly. `gdk4-wayland` gives us the `wl_display`, `wl_seat` and `wl_surface`, and `wayland-client` + `wayland-protocols` bind the two protocols. That's about 300 lines.

X11 support is done with `x11rb` (XI2) on the GDK X11 window.

Exit chord: Right-Super + Right-Alt + Esc. On many Linux layouts Right Alt is AltGr (ISO_Level3_Shift) but still has its own evdev code (100), so it's detectable. Keep the chord configurable.

## 4. Audio, NDI, recording

- **Audio:** cpal → ALSA, which reaches PipeWire/PulseAudio through their ALSA plugins. Check the latency. If ALSA-on-PipeWire adds too much, use the `pipewire` crate directly (cpal has had PipeWire host work in progress).
- **NDI:**
  - Users install the NDI SDK/runtime for Linux (`libndi.so.6`; distro packages exist, e.g. AUR `ndi-sdk`). Core loads it via `NDI_RUNTIME_DIR_V6` or the system path.
  - NDI discovery on Linux relies on **Avahi** (mDNS). Document `avahi-daemon` as a requirement and open UDP 5353 / TCP 5960+ on firewalled distros.
  - **aarch64:** NDI ships Linux aarch64 libraries, so a Raspberry Pi 5 is a viable thin client. Test performance with option (1) above.
- **Recording:** GStreamer via `gstreamer-rs` behind `MediaSink`: `appsrc ! videoconvert ! (vah264enc | x264enc) ! mp4mux` + audio `appsrc ! audioconvert ! avenc_aac/fdkaacenc`. GStreamer is everywhere and picks up VA-API hardware encoders. Fallback: core's ffmpeg sink.

## 5. Other features

- **Settings:** `~/.config/misterkvms/profiles.json` (via core; respects `XDG_CONFIG_HOME`).
- **Icon:** `make_icon.py` also writes PNGs at 16–512 px and the 1024 SVG as `io.github.misterkvms.MiSTerKVMS.svg` (hicolor scalable). Desktop file and AppStream metainfo with screenshots.
- **About:** `adw::AboutWindow` with the same credits, plus a credits section for MiSTer-Kun, the NDI trademark and a link.
- **CLI:** `kvms` from the same build. Needs `ssh`/`scp` (`openssh-client`); `ffmpeg` optional.

## 6. Packaging and distribution

- **Flatpak (Flathub)**, primary:
  - Runtime: `org.gnome.Platform`. Permissions: `--share=network`, `--socket=wayland`, `--socket=fallback-x11`, `--socket=pulseaudio`, `--device=dri`.
  - NDI: either bundle `libndi.so.6` (NDI's license allows redistribution inside the app, with the license text and kept up to date), or load it from an extension/host path. Bundling is friendlier.
  - mDNS works with host networking. Flatpak gives the app the host network namespace, but the Avahi socket may be needed: `--system-talk-name=org.freedesktop.Avahi`.
- **AppImage**, secondary: one file, for people who don't use Flatpak.
- **.deb/.rpm**: later, if asked for.
- **CI:** `ubuntu-latest`, plus an `ubuntu-24.04-arm` runner for aarch64:
  1. `cargo test`
  2. Build the shell and CLI.
  3. Build the Flatpak bundle (`flatpak-builder`) and the AppImage.
  4. Upload to the release.

## 7. Milestones

| # | Milestone | Done when |
|---|---|---|
| L0 | Groundwork (PORTING §2) merged | — |
| L1 | `kvms` Linux builds (x86_64 + aarch64) in releases | CLI works from Linux, including a Pi |
| L2 | Video spike (texture upload) | GTK window shows the NDI stream with crop/scale, 60 fps, measured latency |
| L3 | Input spike (Wayland) | shortcuts inhibit + pointer lock + relative pointer on GNOME and KDE; keys verified on HP9K715 |
| L4 | X11 input | XI2 grabs + raw motion |
| L5 | Shell MVP | sidebar, toolbar, status bar, editor, overlays, predictive cursor, auto-capture |
| L6 | Recording | GStreamer sink |
| L7 | Packaging | Flatpak + AppImage from CI; About, icon, metainfo |
| L8 | Optimisation | dmabuf zero-copy path if L2 measurements call for it |

Estimate: L2–L7 is about 4–6 weeks. Most of the risk is in L2 (video in GTK) and L3 (compositor differences).

## 8. Test plan

- GNOME (Mutter) Wayland, KDE (KWin) Wayland, Sway/Hyprland (wlroots) as best effort, and an X11 session.
- Shortcut inhibit refused: the UI must say "system shortcuts stay with the desktop" (like the macOS no-Accessibility case).
- HiDPI with fractional scaling (125%/150%): picture sharp in Integer mode, pointer lock region correct.
- NVIDIA proprietary driver + Wayland (dmabuf/GL quirks) vs Mesa (Intel/AMD).
- Raspberry Pi 5: CPU use at 1080p60 receive + render, input latency.
- Flatpak sandbox: NDI discovery, audio, settings path, `kvms` CLI inside (`flatpak run --command=kvms`).
