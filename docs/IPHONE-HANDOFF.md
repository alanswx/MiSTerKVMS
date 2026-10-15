# Mac handoff: check the Mac app without NDI, then start the iPhone app

Paste the prompt below into Claude Code on the Mac, in the MiSTerKVMS
checkout.

---

You're continuing work on MiSTerKVMS on `main` (we no longer use feature
branches). `git pull`, then read `README.md` and `docs/CAPTURE-SERVER.md`
(the plan and its decisions), and `git log --oneline -15`.

Rules: commit on `main` as you go with clear messages. **Don't push unless I
say so**: every push to `main` publishes a release. Don't change mrext, never
edit the MiSTers' ini files, and don't change the mkvm protocol without
updating the `mkvm` tests (the server and every app speak it).

## Where things are

- **NDI is gone** (commit "Remove NDI…"). Video comes from MiSTerKVMS Capture
  streams (`mkvm://host/stream`, mDNS `_mkvm._tcp`) or a capture card on the
  viewing computer. Hosts saved with NDI, or with no source, are capture
  hosts: opening one without a stream makes the core find the stream showing
  the same MiSTer address, save it and connect (`core/src/controller.rs`,
  `find_stream_for`).
- **The Mac app's NDI removal was done on Windows and never compiled.**
  Changed: `Model/HostProfile.swift` (sourceKind, subtitle),
  `Views/ProfileEditor.swift` (NDI option and section gone),
  `Views/SessionView.swift` (placeholders, stats line), `Views/AboutView.swift`,
  `Model/Recorder.swift` (comment), `Info.plist` (`_ndi._tcp` removed),
  `MiSTerKVMS.entitlements` (`disable-library-validation` removed; it was only
  for loading libndi). Core API removed: `ndiFindSources`, `ndiRuntimeInfo`,
  `HostProfile.obsHost/ndiSource`, `VideoStats.availableSources`,
  `VideoError.ndiUnavailable`, `VideoSession(ndiSource:…)` constructors.
- Capture server on Windows (DANIHTPC, 192.168.99.82): runs as a service,
  streams `MiSTer-SS1` (UGREEN 25773 → MiSTer 192.168.99.92) and `MiSTer-Pi`
  (GC573 → 192.168.99.143). Setup page http://192.168.99.82:5971 (LAN access
  may be off: it can only be switched on on that PC).
- CLI: `misterkvms-cli streams`, `stream <address>`, and `--stream` /
  `--local` for `screenshot`, `record`, `frames`, `wait-for-change`, `soak`.

## Part 1: the Mac app (do this first)

1. `scripts/build-mac.sh`; fix whatever the Swift changes broke. Keep the UI
   as it is otherwise.
2. Test with the real streams: add a host by picking a discovered stream;
   video, audio, Quality, recording (⌘⇧R), Take control back and forth with
   the Windows app.
3. Migration: a host saved before this change with an NDI source must open,
   say it's looking for its MiSTer's capture stream, then connect and keep
   the stream. (Fake one by editing the settings file: `"source": "ndi"`,
   `"stream": ""`, `"misterHost": "192.168.99.143"`.)
4. A capture card plugged into the Mac (local capture) still works.
5. Signed build: with library validation no longer disabled, check the
   packaged DMG app launches and records (`scripts/package-dmg.sh`).

Report what works and what you fixed before starting part 2.

## Part 2: the iPhone app

**Start by agreeing the scope with me** (ask, don't assume): iPhone only or
iPad too; minimum iOS; how input works (touch as trackpad, on-screen keyboard,
a hardware keyboard/mouse/controller); watching only or full control;
TestFlight/side-loading for now or the App Store. Then write the plan into
`docs/` (like CAPTURE-SERVER.md: decisions table, milestones) before code.

Things I already know you'll hit:

- **The core on iOS:** it must build for `aarch64-apple-ios` and
  `aarch64-apple-ios-sim`. `NativeSurface` only has an AppKit variant: add a
  UIKit one (`raw-window-handle`'s `UiKitWindowHandle`; wgpu draws with Metal
  there too). Audio: cpal works on iOS, but the app must set up an
  `AVAudioSession` (playback). Local capture cards: `capture/` only has
  `cfg(target_os = "macos")` for AVFoundation, so iOS gets the stubs; an
  iPhone can't take a capture card anyway, so leave local capture off on iOS
  (iPadOS 17 can use USB video devices; that's for later).
- **Discovery:** the core finds streams with `mdns-sd`, which uses raw UDP
  multicast. On iOS that needs Apple's multicast entitlement
  (`com.apple.developer.networking.multicast`), which has to be requested.
  Browsing `_mkvm._tcp` in Swift with `NWBrowser` (Network.framework) needs
  no entitlement: prefer that on iOS and pass the address to the core. Either
  way the app needs `NSLocalNetworkUsageDescription` and `NSBonjourServices`
  (`_mkvm._tcp`) in its Info.plist.
- **Input:** with a capture server stream, input goes through the server
  (mkvm `input` messages, control token), so the phone never talks to mrext
  directly. The control banner and Take control work as on the Mac.
- **Building:** the Mac app is a SwiftPM package (`apple/Package.swift`)
  linking `libkvms_core.a` from `scripts/build-mac.sh`. iOS needs an Xcode
  project (an app target can share `KVMSCore`, the UniFFI bindings) and an
  XCFramework of the core for device + simulator. CI doesn't build iOS yet;
  add that only once the app builds locally.
- Keep shared logic in the Rust core (as the Mac, Windows and Linux apps
  do); the iPhone app should be a thin SwiftUI shell.

The plan is to release 1.0 after the iPhone app's first version: the 1.0 bump
is one line (`version = "1.0.0"` in `Cargo.toml`); I'll do it when we're
ready.
