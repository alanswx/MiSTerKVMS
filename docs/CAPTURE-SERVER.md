# Capture server plan: `misterkvms-capture` + OBS plugin

Replace NDI as the video/audio path with our own capture server, and let OBS
*consume* from it through an optional plugin, instead of OBS owning the
capture cards and pushing NDI.

```
Capture PC                                         Any computer on the LAN
┌──────────────────────────────────────┐
│ misterkvms-capture (Rust, headless)  │  TCP     ┌──────────────────────────┐
│  owns the capture cards + their audio│─────────▶│ MiSTerKVMS app (mac/win/ │
│  one stream per MiSTer               │          │ linux) via the core       │
│  MJPEG pass-through / JPEG encode    │─────────▶│ misterkvms-cli (scripts)  │
│  mDNS: _misterkvms._tcp              │          └──────────────────────────┘
└───────────────▲──────────────────────┘
                │ TCP (same protocol)
┌───────────────┴──────────────────────┐
│ OBS + obs-misterkvms plugin (optional)│  "MiSTerKVMS Capture" source:
│  for streaming/recording/scenes       │  the same picture OBS used to get
└──────────────────────────────────────┘  from the card directly
```

Why this way round: a capture card can only be opened by one program. If the
server owns it, the MiSTerKVMS apps, the CLI and OBS can all watch the same
MiSTer at once, and OBS becomes optional. Today OBS owns the card, so
everything depends on OBS and DistroAV/NDI being healthy.

NDI stays as an optional source for people who already run OBS + DistroAV.

Targets: macOS 14+, Windows 10/11, Linux (x86_64 + aarch64) for every piece:
server, apps, CLI and OBS plugin.

## 0. Before anything else: the client memory leak

The client-side leak has to be found whichever video path we use; the new
source would otherwise inherit it.

1. **Measure.** Add `misterkvms-cli soak <minutes>`: runs a full
   `Controller` session (input + video, offscreen renderer) and logs RSS,
   frame counts and NDI reconnects every 10 s. Run it on each OS, including
   one run against a flapping source (your Mac's current state).
2. **Suspects**, in order:
   - Receiver churn: every NDI reconnect allocates; check that nothing grows
     per reconnect (frame `Vec`s in the slot, audio ring, cursor detector).
   - The macOS shell: a `CAMetalLayer` left per host switch (now removed on
     switch; verify), AVFoundation recorder buffers, SwiftUI state growth
     from the 2 Hz `Changed` events.
   - wgpu: textures recreated per resolution change; command buffers piling
     up when the view is occluded (`Occluded` skips present).
3. **Fix and guard:** a soak run in CI (Linux, offscreen, synthetic source,
   10 minutes) that fails if RSS grows more than a set amount after warm-up.

Done when: 8-hour soak on the Mac with stable memory.

## 1. Names

| Piece | Name | Notes |
|---|---|---|
| Desktop app | MiSTerKVMS | unchanged |
| Command-line tool | `misterkvms-cli` (was `kvms`) | `kvms` kept as an alias for one release, with a deprecation note |
| Capture server | `misterkvms-capture` | headless; runs as a service |
| OBS plugin | `obs-misterkvms` | separate build, GPL (see §6) |
| Protocol | MiSTerKVMS Stream Protocol (MKSP) v1 | |

## 2. Protocol (MKSP v1)

Plain TCP, one connection per stream subscription. LAN only, no
auth/TLS, like mrext (§ Security in PLANNING.md).

- **Discovery:** mDNS service `_misterkvms._tcp` (TXT: server version,
  stream names); manual `host[:port]` always works. Default port 5970.
- **Handshake:** the client sends a JSON `hello` (protocol version, client
  name, what it can decode); the server answers with its streams (id, name,
  current resolution, fps, audio format). The client sends `subscribe
  {stream, video: true, audio: true, max_fps?}`.
- **Frames:** binary, length-prefixed:
  `type u8 | stream u8 | flags u16 | timestamp i64 (100 ns, server clock) | len u32 | payload`.
  - `video`: one JPEG (MJPEG frame passed through, or encoded by us) plus
    width/height in a small prefix.
  - `audio`: interleaved s16 or f32 PCM, 48 kHz, channel count from the
    stream info.
  - `format`: resolution/fps changed (MiSTer switched modes).
  - `ping`/`pong`: keep-alive and **clock offset**, so the "frame age"
    stat no longer depends on NTP between machines.
- **Back-pressure:** the server never blocks capture on a slow client. Per
  client it keeps only the newest video frame and a short audio queue; if
  the socket is full, older video frames are dropped and counted (reported
  in stats). Audio is never dropped silently: an overflow sends a `gap`.
- **Versioning:** major version in `hello`; unknown frame types are skipped,
  so later additions (H.264, metadata) don't break old clients.

Bandwidth at 1080p60: MJPEG/JPEG at quality ~85 is roughly 60–120 Mbit/s.
That's fine on gigabit and keeps latency to about one frame (no
inter-frame coding). H.264/HEVC through hardware encoders is a later option
for Wi-Fi clients (§8).

The protocol lives in a new crate, `mksp/`, used by the server, the core
(client) and the OBS plugin, with round-trip tests and a fuzz target for the
frame parser.

## 3. `misterkvms-capture`

A new crate `capture/`, sharing `mksp` and parts of the core (logging,
config folder).

**Video capture per OS**, behind one `CaptureDevice` trait (list devices
and formats, open with a format, deliver frames with timestamps):

| OS | API | Notes |
|---|---|---|
| Windows | Media Foundation `IMFSourceReader` (`windows` crate) | Same API OBS uses for most cards (OBS also uses DirectShow for some; MF covers UVC and AVerMedia). |
| macOS | AVFoundation `AVCaptureSession` (`objc2-av-foundation`) | Needs Camera permission (the usage description in Info.plist). |
| Linux | V4L2 (`v4l` crate) | mmap streaming. |

Spike first: the `nokhwa` crate wraps all three; if it gives us format
control (pick MJPEG vs raw, exact resolution/fps) and per-frame timestamps,
it saves most of this. If not, use the APIs directly.

**Formats:** prefer the card's own MJPEG (UGREEN-class USB sticks) and pass
frames through untouched (no decode, no re-encode). Raw formats (NV12/YUY2,
e.g. AVerMedia GC573) are JPEG-encoded per frame (`turbojpeg`, ~3–5 ms at
1080p on one core; one encoder thread per stream).

**Audio:** capture cards appear as a separate audio input device; capture
with `cpal` (WASAPI / CoreAudio / ALSA). The config pairs each video device
with its audio device (matched by name automatically when obvious).

**Configuration:** `capture.json` in the config folder:
`streams: [{ name: "MiSTer-SS1", video: {device, format, width, height, fps},
audio: {device} }]`, `port`. Commands:

- `misterkvms-capture devices`: list devices and their formats (JSON).
- `misterkvms-capture add "MiSTer-SS1" --video "UGREEN 25773" --audio auto`
- `misterkvms-capture serve`: run (also what the service runs).
- `misterkvms-capture install-service` / `uninstall-service`: Windows
  service, launchd agent, or systemd user unit.

A small status page (`http://<pc>:5970/` on the same port, sniffed by the
first bytes) shows streams, clients, fps and drops, so headless boxes can be
checked from a browser. A tray app or GUI editor can come later.

**Robustness:** a device that disappears or fails to configure (the
"Video format match failed" seen in OBS with the UGREEN) is retried with
backoff, and clients get a `format`/`gap` frame instead of a dead socket.
Resolution changes from the MiSTer are followed automatically.

## 4. Client: the core

- New `video::source` module: a `FrameSource` trait with the receive loop's
  needs (next video frame as BGRA or JPEG, audio, connection state, stats).
  Two implementations: `NdiSource` (today's code, moved) and `MkspSource`.
- JPEG decode in the receive thread with `zune-jpeg` (pure Rust, SIMD) into
  the existing frame slot; the renderer, auto-crop, predictive cursor,
  recording and stats stay as they are. GPU decode is a later optimisation if
  CPU use matters (Raspberry Pi).
- `HostProfile` gets a source: `{ kind: "ndi", name, obsHost }` or
  `{ kind: "capture", host, stream }`. Old profiles map to `ndi`.
- Discovery in the host editor lists both: "Capture server streams" and
  "NDI sources".
- Stats overlay: per-source fields (server drops, clock-corrected age).
- `misterkvms-cli` screenshot/record/frames work with either source.

The three shells only change their host editors (source picker); the rest
comes through the controller.

## 5. OBS plugin `obs-misterkvms`

An OBS **source** ("MiSTerKVMS Capture"): pick a server and stream; it
connects with MKSP and outputs the video and audio as an async source, so
OBS scenes, filters, recording and streaming keep working while the server
owns the card.

- **Structure:** a C plugin shell (from `obs-plugintemplate`: CMake, builds
  against OBS 30+ on all three OSes) that calls a small Rust static library
  (`mksp-client` with a C ABI: connect, poll decoded frame as NV12/BGRA,
  poll audio, stats). One protocol implementation for every client.
- **Properties:** server (discovered list + manual), stream, "decode to
  NV12" (cheaper for OBS) and a stats line.
- **Optional, later:** an OBS *output/filter* that pushes OBS's own output
  into `misterkvms-capture` as an extra stream, for people who want OBS
  compositing in the KVM view. That would fully replace DistroAV for this
  use.
- **Packaging:** Windows installer/zip into the OBS plugins folder, macOS
  `.pkg` (signed + notarised with the existing Developer ID), Linux `.deb`
  plus a tarball; CI from the plugin template's workflows.

## 6. Licensing

- Server, core, apps, CLI, `mksp`: MIT, as now.
- The OBS plugin links libobs (GPL-2.0), so the plugin is **GPL-2.0-or-later**
  in its own folder (`obs-plugin/`) with its own LICENSE. The MIT Rust
  library it uses is GPL-compatible. The apps only speak the protocol to the
  server, so they're unaffected.
- Dropping NDI as the default also removes the NDI runtime requirement and
  its redistribution rules for users who don't need it.

## 7. Milestones

| # | Milestone | Done when |
|---|---|---|
| C0 | Client leak found and fixed | 8 h Mac soak stable; CI soak guard |
| C1 | Rename CLI to `misterkvms-cli` | builds/docs/CI updated; `kvms` alias works |
| C2 | `mksp` crate | spec in this doc, round-trip tests, fuzzed parser |
| C3 | Capture spike | `nokhwa` vs native decided; UGREEN MJPEG pass-through and GC573 raw capture at 1080p60 on Windows |
| C4 | `misterkvms-capture` on Windows | serve + devices + config; audio; status page; service install |
| C5 | Core `MkspSource` | Mac/Windows/Linux apps show a server stream; latency measured vs NDI (expect equal or lower) |
| C6 | Host editor source picker in all three apps | discovery of both kinds; profiles migrate |
| C7 | Server on macOS and Linux | same features; Raspberry Pi 5 as a capture box tested |
| C8 | OBS plugin (source) | Win/mac/Linux builds in CI, OBS shows the stream with audio |
| C9 | Packaging | server in releases (zip/pkg/tarball + service), plugin installers |
| C10 | Later | H.264 option for Wi-Fi, OBS output/filter, tray app |

Rough size: C0 a few days; C1–C6 about two weeks; C7–C9 another two.
C3 (capture APIs) and C8 (OBS build matrix) carry most of the risk.

## 8. Risks and open questions

- **Device access on macOS** needs the Camera permission for the server,
  which means it has to be a signed app bundle (or a launchd agent inside
  one), not a bare binary.
- **AVerMedia GC573 on Mac/Linux:** driver support varies; the Windows PC
  is the realistic capture box for that card.
- **Wi-Fi clients:** MJPEG at 1080p60 is too heavy for many Wi-Fi links;
  that's when H.264 (hardware encode on the server, VideoToolbox/MF/VA-API
  decode on clients) is worth adding. Also a per-client `max_fps`/quality.
- **OBS and the server on the same PC:** the plugin connects over loopback;
  measure CPU (decode in OBS) and consider passing raw NV12 over a shared
  memory ring for the local case later.
- **mDNS on Windows** works without Bonjour on Windows 10+ (`mdns-sd` crate
  does its own multicast), but firewall rules for UDP 5353 and TCP 5970 are
  needed: the service installer adds them.
