# Mac handoff: update the client and test it

Paste the prompt below into Claude Code on the Mac, in the MiSTerKVMS
checkout.

---

You're continuing work on MiSTerKVMS on the `native-protocol` branch, started
on Windows. Read `docs/CAPTURE-SERVER.md` first: it's the plan and the
decisions (R1–R13). Then `git log --oneline main..native-protocol` for what's
been done. Work on this branch; commit as you go with clear messages.

## Where things are

NDI is being replaced by our own capture server and protocol:

- `mkvm/`: the protocol (messages, `mkvm://host/stream` addresses, control
  token).
- `capture/`: capture cards. Windows: Media Foundation + DirectShow (tested
  with a UGREEN 25773 and an AVerMedia GC573). **macOS: AVFoundation in
  `capture/src/avf.rs`, written and type-checked from Windows but never
  run.** Also a built-in test pattern (`test:pattern`, listed when
  `MISTERKVMS_TEST_PATTERN=1`).
- `codec/`: JPEG, NV12/YUY2 conversion, scaling.
- `server/`: `misterkvms-capture` (streams, setup page on port 5971, mDNS).
- `core/`: the apps' shared core. Three video sources per host:
  `source` = "capture" (a server stream, `stream` = mkvm address), "local"
  (`localVideo`/`localAudio` = a card on this computer), "ndi" (legacy).
  Empty `source` is inferred from which field is filled.
  Controller additions: `take_control()`, `viewer_name()` /
  `set_viewer_name()`, `set_quality("auto"|"best"|"balanced"|"low")`;
  `ControllerState.stream` (`StreamState`: connected, error,
  mister_connected, have_control, holder, round_trip_ms). Free functions:
  `find_streams(wait_ms)`, `capture_cards()`, `audio_inputs()`.
- `cli/`: now `misterkvms-cli` (`kvms` still works). Useful here:
  `streams`, `stream <address>`, `soak <duration> --local <card>|--stream <address>`.
- The Windows app already has the new UI: see
  `windows/MiSTerKVMS/HostEditorDialog.cs` and `MainWindow.xaml(.cs)`
  (source picker, quality, "X has control" banner with Take control,
  viewer name). Mirror it in SwiftUI.

## Tasks, in order

1. **Build.** `cargo build --workspace --exclude kvms-linux` and
   `cargo test --workspace --exclude kvms-linux`. Fix anything macOS-specific.
2. **AVFoundation backend.** Plug in a capture card (or use the built-in
   camera to start). Run `cargo run --release -p kvms-capture --example cards`
   and `--example close -- <part of the card name>`. Expect formats listed,
   frames at the right rate, a clean close. Camera permission: a bare
   binary run from Terminal asks on behalf of Terminal; allow it. Fix what
   doesn't work (format/rate selection, BGRA output, stride, the delegate,
   stop on drop). Card audio on macOS is a normal CoreAudio input
   (`audio_devices()`), not a pin.
3. **Server on the Mac.** `MISTERKVMS_TEST_PATTERN=1 cargo run --release -p misterkvms-capture -- add Test --video test:pattern --audio none`,
   then `... -- serve`, open http://localhost:5971, then
   `cargo run --release -p kvms-cli -- stream mkvm://127.0.0.1/Test --duration 5s --save /tmp/f.png`.
   Then a stream for the real card.
4. **Regenerate bindings and update the Swift app** (`scripts/build-mac.sh`
   regenerates `apple/Sources/KVMSCore/Generated/` with uniffi). Keep the
   current UI; change only (R12):
   - `Views/ProfileEditor.swift`: "Video & audio from" picker: MiSTerKVMS
     Capture stream (list from `findStreams`, editable address, Quality) /
     Capture card on this Mac (`captureCards`, `audioInputs`) / OBS via NDI
     (existing fields). Hide the MiSTer fields for capture streams (the
     server knows the MiSTer). New hosts default to capture streams.
   - Session view: when `state.stream.holder` is non-empty and
     `haveControl` is false, a banner "<holder> has control" with **Take
     control** (calls `takeControl()` then starts capture). Clicking the
     video must not start capture while someone else has control.
     Status bar: "You have control" / "Watching · X has control".
   - View menu: Quality (Automatic, Best, Balanced, Low) for capture
     streams; settings: Viewer name.
   - Placeholder texts per source, as in the Windows app.
   - Host list subtitle: stream name / "Capture card on this Mac" / MiSTer
     address.
   - `apple/Info.plist`: add `NSCameraUsageDescription` and
     `NSMicrophoneUsageDescription` (local capture cards need them).
5. **Test the app.** Against the Mac's own server (test pattern, then the
   card) and, if it's reachable, the Windows capture PC's server (its
   streams show up in the stream list via mDNS). Check: video, audio,
   Take control between two viewers (the Mac app and
   `misterkvms-cli stream` or the Windows app), quality switching, local
   capture card. Run `misterkvms-cli soak 10m --local <card>` and
   `--stream <address>`: memory must stay flat.
6. Update `docs/CAPTURE-SERVER.md` with what you found (milestones C4, C7, C9
   for macOS), and report what works, what doesn't, and anything that
   needs a decision.

## Notes

- The Mac app is direct download only for now (R11); no App Store sandbox
  concerns.
- Don't change the protocol without updating `mkvm` tests; the Windows
  server and apps speak it too.
- If the Windows capture PC is on the network, its setup page is only
  reachable from that PC unless `allow_lan` is on in its `capture.ini`;
  streams themselves are always reachable on TCP 5970.
