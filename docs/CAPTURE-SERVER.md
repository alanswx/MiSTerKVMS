# Capture server plan: `misterkvms-capture` replaces NDI

Replace NDI with a capture server we own. It owns the capture cards and
streams to our apps, the CLI and an optional OBS plugin over our own protocol
(MKSP). Nothing to install besides our own programs: no NDI runtime, no
DistroAV, no RustDesk.

Decisions so far (October 2026):

| # | Decision |
|---|---|
| R1 | The capture server owns the capture cards; OBS becomes an optional consumer through a plugin. |
| R2 | Our own protocol, MKSP, for everything. RustDesk was considered and dropped: it would make the project AGPL and tie us to an unpublished protocol, for stock viewers we don't need now. |
| R3 | The project stays **MIT** (D15). Codecs come from OS encoders and permissively licensed libraries only (§6). |
| R4 | Downscaling (e.g. to 1080p) is chosen **per viewer** (§5). |
| R5 | NDI is **removed** once the capture server ships (C9); until then it stays as a legacy source so nobody is stranded. |
| R6 | First, find and fix the client-side memory leak (§1). |

```
Capture PC                                          Any computer on the LAN
┌─────────────────────────────────────────┐
│ misterkvms-capture (Rust, headless)     │  MKSP     ┌───────────────────────────┐
│  capture cards + audio                  │  (TCP)    │ MiSTerKVMS apps (mac/win/  │
│  per stream: MJPEG pass-through or      │──────────▶│ linux): predictive cursor, │
│  encode; per-viewer scale (§5)          │           │ integer scale, crop, stats │
│  mDNS: _misterkvms._tcp                 │──────────▶│ misterkvms-cli             │
│  status page on the same port           │           └───────────────────────────┘
└───────────────▲─────────────────────────┘
                │ MKSP (loopback or LAN)
┌───────────────┴─────────────────────────┐
│ OBS + obs-misterkvms plugin (optional)  │  "MiSTerKVMS Capture" source: the
│  for streaming / recording / scenes     │  picture OBS used to get from the card
└─────────────────────────────────────────┘
```

Why this way round: a capture card can only be opened by one program. If the
server owns it, the apps, the CLI and OBS can all watch the same MiSTer at
once, and OBS becomes optional. Today OBS owns the card, so everything
depends on OBS and DistroAV/NDI being healthy.

Input is unchanged: the apps keep talking to mrext on the MiSTer directly.

Targets: macOS 14+, Windows 10/11, Linux (x86_64 + aarch64) for every piece.

## 1. First: the client memory leak

Whatever the video path, the leak has to go first; the new source would
inherit it.

1. **Measure:** `misterkvms-cli soak <minutes>` runs a full `Controller`
   session (input + video, offscreen renderer) and logs RSS, frame counts
   and reconnects every 10 s. Run it on each OS, including against a
   flapping source like the Mac's current NDI link.
2. **Suspects:** allocations per reconnect (frame buffers, audio ring,
   cursor detector); the macOS shell (Metal layers per host switch, the
   AVFoundation recorder, SwiftUI churn from the 2 Hz state events); wgpu
   textures per resolution change; work queued while the view is occluded.
3. **Guard:** a 10-minute soak in CI (Linux, offscreen, synthetic source)
   that fails if memory keeps growing after warm-up.

Done when an 8-hour soak on the Mac holds steady.

## 2. Names

| Piece | Name |
|---|---|
| Desktop apps | MiSTerKVMS |
| Command-line tool | `misterkvms-cli` (was `kvms`; alias kept for one release) |
| Capture server | `misterkvms-capture` |
| OBS plugin | `obs-misterkvms` |
| Protocol | MiSTerKVMS Stream Protocol (MKSP) v1 |

## 3. Protocol (MKSP v1)

Plain TCP, one connection per stream subscription. LAN only, no auth/TLS,
like mrext (§ Security in PLANNING.md); away-from-home use is out of scope
for v1 (§8).

- **Discovery:** mDNS `_misterkvms._tcp` (TXT: server version, stream
  names); manual `host[:port]` always works. Default port 5970.
- **Handshake:** the client sends a JSON `hello` (protocol version, client
  name, codecs it can decode); the server answers with its streams (id,
  name, native resolution, fps, audio format, offered resolutions). The
  client sends `subscribe {stream, video, audio, max_size?, max_fps?, codec?}`
  and may send it again at any time to change size or codec (§5).
- **Frames:** binary, length-prefixed:
  `type u8 | stream u8 | flags u16 | timestamp i64 (100 ns, server clock) | len u32 | payload`.
  - `video`: codec id, width/height, keyframe flag, then the payload
    (a JPEG, or an H.264/H.265 access unit).
  - `audio`: interleaved s16 PCM, 48 kHz, channel count from the stream info.
  - `format`: resolution/fps changed (MiSTer switched modes) or no signal.
  - `ping`/`pong`: keep-alive and **clock offset**, so the "frame age" stat
    doesn't depend on NTP between machines.
  - `gap`: frames or audio were dropped (with counts).
- **Back-pressure:** the server never blocks capture on a slow client. Per
  client it keeps only the newest video frame (MJPEG) or skips to the next
  keyframe (H.264) and a short audio queue; drops are counted and reported.
- **Versioning:** major version in `hello`; unknown frame types are skipped,
  so later additions don't break old clients.

The protocol lives in a new crate, `mksp/`, used by the server, the core
and the OBS plugin, with round-trip tests and a fuzz target for the parser.

## 4. The capture server

### 4.1 Capture

Behind one `CaptureDevice` trait (list devices/formats, open with a format,
frames with timestamps):

| OS | API |
|---|---|
| Windows | Media Foundation `IMFSourceReader` (also gives hardware scaling and colour conversion) |
| macOS | AVFoundation `AVCaptureSession` (needs Camera permission: the server ships as a signed app bundle) |
| Linux | V4L2 |

The spike also tries the `nokhwa` crate, which wraps all three, if it gives
format control (MJPEG vs raw, exact size/fps) and per-frame timestamps.
Audio from the card's own audio device through `cpal`; each stream pairs a
video and an audio device (matched by name when obvious). A device that
drops out or fails to configure (as the UGREEN did in OBS) is retried with
backoff; viewers get a `format` "no signal" frame, not a dead socket.
Resolution changes from the MiSTer are followed automatically.

### 4.2 Encoding

Two codecs, chosen per viewer:

- **MJPEG (default on wired LAN).** Cards that deliver MJPEG (UGREEN-class
  USB sticks) are passed through untouched at native size. Raw cards
  (NV12/YUY2, e.g. AVerMedia GC573) are JPEG-encoded per frame
  (`turbojpeg`, ~3–5 ms at 1080p). No inter-frame coding, so latency is
  about one frame and every frame stands alone. 1080p60 at quality ~85 is
  roughly 60–120 Mbit/s: fine on gigabit.
  JPEG can be encoded 4:4:4, which keeps coloured text and pixel art sharp.
- **H.264 (for Wi-Fi and scaled viewers).** Hardware encoders through the
  OS: Media Foundation (NVENC/Quick Sync/AMF behind it) on Windows,
  VideoToolbox on macOS, VA-API on Linux; `openh264` (BSD) as the software
  fallback. Low-latency settings: no B-frames, short GOP, one-frame delay,
  keyframe on request. H.265 later if it pays off.

Clients decode H.264 with the same OS APIs (VideoToolbox, Media Foundation,
VA-API) or `openh264`, and JPEG with `zune-jpeg`.

### 4.3 Configuration and running

`capture.json` in the config folder:
`streams: [{ name: "MiSTer-SS1", video: {device, format, width, height, fps},
audio: {device} }]`, `port`. Commands:

- `misterkvms-capture devices`: list devices and their formats (JSON).
- `misterkvms-capture add "MiSTer-SS1" --video "UGREEN 25773" --audio auto`
- `misterkvms-capture serve`: run (also what the service runs).
- `misterkvms-capture install-service` / `uninstall-service`: Windows
  service, launchd agent, or systemd unit; also adds firewall rules (TCP
  5970, UDP 5353 for mDNS).

A status page (`http://<pc>:5970/`, same port, sniffed from the first bytes)
shows streams, viewers, fps, encoder and drops, so headless boxes can be
checked from a browser. A tray app or GUI editor can come later.

## 5. Per-viewer downscaling

Each viewer chooses its resolution:

- The server advertises the offered sizes per stream: native, 1080p, 720p
  and the integer fractions of native (4K → 1080p is exactly 2:1).
- The client asks with `max_size` in `subscribe`. Our apps set it from a
  per-host "Max resolution" (Native / 1080p / 720p / Auto). Auto picks
  1080p and H.264 when the link measures below a threshold.
- The server keeps one scale + encode pipeline per distinct (size, codec);
  viewers asking for the same share one. Native MJPEG pass-through costs
  nothing. Scaling: exact box filter for integer ratios (keeps pixel art
  crisp), area filter otherwise; on the GPU or Media Foundation's video
  processor where available.
- Encoder load rises with each distinct pipeline, so the server caps how
  many it runs at once (configurable; default 3).

## 6. Licensing

- Server, core, apps, CLI, `mksp`: **MIT**, as now (D15). Mac App Store
  stays possible.
- Dependencies stay permissive: OS encoders/decoders, `turbojpeg`
  (BSD/IJG), `zune-jpeg` (MIT/Apache), `openh264` (BSD; Cisco's binary
  covers the H.264 patent licence where needed). No ffmpeg GPL builds.
- The OBS plugin links libobs (GPL-2.0), so the plugin is
  **GPL-2.0-or-later** in its own folder (`obs-plugin/`) with its own
  LICENSE. The MIT library it uses is GPL-compatible; the apps only speak
  the protocol, so they're unaffected.
- With NDI removed (R5), the NDI runtime, its SDK licence and its
  redistribution rules go away.

## 7. Client side

### 7.1 The core

- New `video::source` module: a `FrameSource` trait (next frame, audio,
  connection state, stats). `MkspSource` is the new one; today's NDI code
  moves behind it as `NdiSource` until R5 removes it.
- Decode in the receive thread into the existing frame slot, so the
  renderer, auto-crop, predictive cursor, recording and stats stay as they
  are.
- `HostProfile` gets a source: `{ kind: "capture", host, stream }`, or the
  legacy `{ kind: "ndi", ... }`. Old profiles keep working until R5, then
  the app offers to switch them to a discovered capture stream.
- Stats overlay: codec, size, server drops, clock-corrected age.
- `misterkvms-cli` screenshot/record/frames work with either source.

The three shells only change their host editors (source picker, max
resolution); the rest comes through the controller.

### 7.2 OBS plugin `obs-misterkvms`

An OBS **source** ("MiSTerKVMS Capture"): pick a server and stream; it
outputs video and audio as an async source, so OBS scenes, recording and
streaming keep working while the server owns the card.

- A C plugin shell (from `obs-plugintemplate`, OBS 30+) over a Rust static
  library with a small C API (the same `mksp` client code as the apps).
- On the same PC as the server it connects over loopback and asks for
  native MJPEG; a shared-memory path for raw frames is a later optimisation.
- Packaging: Windows zip/installer, signed + notarised macOS `.pkg`, Linux
  `.deb` and tarball, from the plugin template's CI.

## 8. Milestones

| # | Milestone | Done when |
|---|---|---|
| C0 | Client leak fixed | 8 h Mac soak steady; CI soak guard |
| C1 | CLI renamed to `misterkvms-cli` | builds, docs, CI updated; `kvms` alias works |
| C2 | `mksp` crate | spec as in §3, round-trip tests, fuzzed parser |
| C3 | Capture spike | `nokhwa` vs native decided; UGREEN (MJPEG) and GC573 (raw) at 1080p60 on Windows |
| C4 | `misterkvms-capture` on Windows, MJPEG | several streams, audio, status page, service + firewall |
| C5 | Core `MkspSource` + host editors | Mac/Windows/Linux show a server stream; latency measured against NDI (expect equal or lower) |
| C6 | H.264 + per-viewer resolution | hardware encode on Windows, decode in all three apps; shared pipelines; integer box filter |
| C7 | Server on macOS and Linux | incl. Raspberry Pi 5 as a capture box |
| C8 | OBS plugin | Win/mac/Linux builds; OBS shows a stream with audio |
| C9 | Packaging; NDI removed | server and plugin in releases; NDI code, docs (SETUP-NDI) and release notes removed; profiles migrate |

Rough size: C0 a few days; C1–C5 about two weeks; C6–C9 about three more.
The biggest risks are C3 (capture APIs and card quirks) and C6 (hardware
codecs on three OSes).

## 9. Risks and open questions

- **Latency vs NDI:** MJPEG should match or beat NDI on the LAN (both are
  intra-frame). H.264 adds a few milliseconds of encode and decode; measure
  end to end (the predictive cursor hides some of it).
- **macOS capture box:** Camera permission and a signed bundle; capture
  card drivers vary, so the Windows PC stays the realistic capture box.
- **AVerMedia GC573 on Mac/Linux:** driver support varies.
- **Away from home and phones:** not in v1. Options later: MKSP over
  Tailscale/WireGuard (works today, just needs auth added), or a WebRTC
  output for browsers and phones. RustDesk compatibility stays possible as
  an add-on, but would bring AGPL with it.
- **OBS and the server on one PC:** measure the plugin's decode CPU; the
  shared-memory path in §7.2 is the fix if needed.
