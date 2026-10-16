# Capture server plan: `misterkvms-capture` replaces NDI

Replace NDI with a capture server we own. It owns the capture cards and
streams each one to any number of viewers over our own protocol (MKVM).
Nothing to install besides our own programs: no NDI runtime, no DistroAV,
no OBS, no RustDesk. A card plugged into the viewing computer itself works
with no server at all, using the same capture code.

Decisions so far (October 2026):

| # | Decision |
|---|---|
| R1 | A Rust capture server owns the capture cards. The clients stay native (Swift / WinUI / GTK) on the shared Rust core. |
| R2 | Our own protocol, MKVM. RustDesk was considered and dropped: AGPL and an unpublished protocol, for stock viewers we don't need now. |
| R3 | The project stays **MIT** (D15). Codecs come from OS encoders and permissively licensed libraries only (§9). |
| R4 | Resolution and codec are chosen **per viewer** (§6). |
| R5 | NDI is **removed** (done): no NDI code, runtime or SDK in the apps or the CLI. |
| R6 | The client leak is fixed (25eecb5: uploads flushed when a frame isn't presented). What's left is a soak guard so it stays fixed (§2). |
| R7 | **Local capture**: a client can read a card plugged into the same machine, through the same `capture` and `codec` crates as the server (§3). |
| R8 | **Several viewers per stream**, one of them in control at a time (§7). Input goes through the server, which keeps the one mrext connection per MiSTer. |
| R9 | Each card is its **own stream**, addressable and discoverable on its own, like an HDHomeRun tuner (§5.1). |
| R10 | One optional password, for the setup page only, kept in plain text in `capture.ini` so it's easy to reset. Streams have no passwords on the home network. From the internet (opt-in per capture PC), only paired devices get in, encrypted with a key of their own (IPHONE.md §2–3). |
| R11 | Direct download first. Mac App Store, the OBS plugin and the Raspberry Pi capture box come later. |
| R12 | Client UI/UX stays as it is on the Mac; only the host editor, a control indicator and the stats overlay change (§8.3). |
| R13 | The protocol is called **mkvm** (crate `mkvm`, URLs `mkvm://`, mDNS `_mkvm._tcp`). |

```
Capture PC                                             Any computer on the LAN
┌──────────────────────────────────────────┐
│ misterkvms-capture (service)             │  MKVM      ┌──────────────────────────┐
│  stream "MiSTer-SS1"  ◀── card 1 + audio │  TCP 5970  │ MiSTerKVMS (mac/win/linux)│
│  stream "MiSTer-HP"   ◀── card 2 + audio │◀──────────▶│  video, audio, input,     │
│   per stream: viewers, control token,    │  video ▶   │  control indicator        │
│   mrext session, encode pipelines        │  ◀ input   ├──────────────────────────┤
│  setup page + previews  (HTTP 5971)      │            │ misterkvms-cli            │
│  mDNS: one entry per stream              │            └──────────────────────────┘
└───────────────┬──────────────────────────┘
                │ mrext WebSocket (one per stream)
                ▼
             MiSTers

Viewing computer with the card plugged in (no server):
  MiSTerKVMS ── capture + codec crates (in-process) ── card;  input ── mrext directly
```

Targets: macOS 14+, Windows 10/11, Linux x86_64 (aarch64 with the Pi, later).

## 1. Code layout

The pieces that touch video live in shared crates, so the server, the
clients' local capture and the CLI run the same code:

| Crate | Kind | Contents | Used by |
|---|---|---|---|
| `capture/` | lib | One `VideoCapture` over per-OS backends: Media Foundation (USB sticks), DirectShow (cards Media Foundation can't open, e.g. GC573, including their audio pin), AVFoundation (macOS), V4L2 (Linux, raw ioctls, MJPEG passed through); audio via `cpal`; stable device IDs; "in use by another app" detection | server, core (local capture), CLI |
| `codec/` | lib | JPEG decode (`zune-jpeg`) and encode (`turbojpeg`); H.264 encode/decode through the OS (Media Foundation, VideoToolbox, VA-API) and `openh264`; scaling (integer box, area) | server, core |
| `mkvm/` | lib | protocol types, frame parser, client and server connection state machines, control token logic | server, core, CLI, later the OBS plugin |
| `server/` | bin `misterkvms-capture` | streams, viewers, pipelines, mrext sessions, setup page, service install | — |
| `core/` | lib (existing) | `FrameSource` trait with `MkvmSource`, `LocalSource`; controller, renderer, input | the three apps, CLI |

The control token rules (§7) live in `mkvm` as a plain state machine with
unit tests, so the server enforces them and the client shows the same state
without duplicating logic.

## 2. Keeping the leak fixed

The leak was the renderer keeping every frame upload while the window was
hidden (fixed in 25eecb5). To keep it that way:

- `misterkvms-cli soak <minutes>`: a full `Controller` session (offscreen
  renderer, synthetic or real source) logging memory, frames and reconnects
  every 10 s, including stretches where the target reports "occluded".
- CI runs a 10-minute soak on Linux and fails if memory keeps growing after
  warm-up. Later it also runs against a local `misterkvms-capture` with a
  synthetic card.

## 3. Local capture (card in the viewing computer)

- In the host editor a source can be **"Capture card on this computer"**:
  pick the card and its audio from the devices `capture` lists, with the
  same preview as the server's setup page.
- `LocalSource` opens the card through `capture` and hands frames to the
  renderer directly: raw frames as they are, MJPEG through `codec`'s JPEG
  decoder. No encode, no network: the lowest latency path there is.
- Input goes straight to mrext, as today.
- The card can only be open in one program at a time. If the server (or
  OBS) has it, the editor says so. A later option: "Share this card on the
  network" runs the server's stream code in-process.
- macOS: the app gains Camera and Microphone usage descriptions and asks
  when a local source is first used. Windows: the camera privacy setting for
  desktop apps must be on; the app links to it when access is refused.

## 4. Protocol (MKVM v1)

One TCP connection per viewer per stream, `TCP_NODELAY`. No TLS on the LAN.

- **Handshake:** the client sends a JSON `hello` (protocol version, viewer
  name, platform, decodable codecs); the server answers
  with the stream's info (name, MiSTer name, native resolution, fps, audio
  format, offered sizes and codecs, control state). The client sends
  `subscribe {video, audio, max_size?, max_fps?, codec?}`, and may send it
  again at any time to change quality (§6) without reconnecting.
- **Server → client frames,** binary, length-prefixed:
  `type u8 | flags u8 | len u32 | timestamp i64 (100 ns, server clock) | payload`.
  - `video`: codec id, width/height, keyframe flag, payload (a JPEG or an
    H.264 access unit).
  - `audio`: interleaved s16 PCM, 48 kHz.
  - `format`: resolution/fps changed, or no signal.
  - `control`: who controls the stream now (§7).
  - `mister`: mrext connection state for the stream.
  - `ping`/`pong`: keep-alive and clock offset (frame age without NTP).
  - `gap`: frames or audio dropped, with counts.
- **Client → server:** `subscribe`, `input` (the core's input messages:
  key, mouse move, buttons, release-all), `take_control`, `pong`.
- **Back-pressure:** capture never waits for a slow viewer. Per viewer the
  server keeps only the newest MJPEG frame, or skips to the next keyframe
  (H.264), and a short audio queue; drops are counted. Input is never
  dropped.
- **Versioning:** major version in `hello`; unknown message types are
  skipped.
- **Transport later:** QUIC on UDP 5970 (`quinn`, MIT/Apache) for Wi-Fi
  and away from home: no head-of-line blocking, encryption built in. The
  connection code sits behind a trait so this is an addition, not a rewrite.

## 5. The capture server

### 5.1 Streams

- One stream per card: a video device, an audio device, a name and the
  MiSTer it shows (mrext address).
- **Each stream is its own thing on the network,** like an HDHomeRun tuner:
  - its own mDNS instance, `MiSTer-SS1._mkvm._tcp` (TXT: stream id,
    MiSTer name, resolution, server version), so clients list MiSTers, not
    servers;
  - its own address, `mkvm://<pc>:5970/MiSTer-SS1`, usable by hand when
    discovery doesn't work;
  - its own viewers, control token and mrext connection.
  One stream failing (card unplugged) never affects the others.
- Cards are identified by **hardware ID** (Windows device path, AVFoundation
  `uniqueID`, `/dev/v4l/by-id`), not name, so two identical UGREEN sticks
  stay apart. The setup page's previews show which is which.

### 5.2 Capture and encoding

- Capture through `capture` (§1). A card that disappears or fails to
  configure is retried with backoff; viewers get "no signal", not a dead
  connection. MiSTer resolution changes are followed automatically.
- **MJPEG** (default on wired LAN): MJPEG cards are passed through
  untouched at native size; raw cards (e.g. AVerMedia GC573) are encoded
  per frame, 4:4:4. About one frame of latency; 1080p60 is 60–120 Mbit/s.
- **H.264** (Wi-Fi, scaled viewers): hardware through the OS, `openh264`
  as fallback; no B-frames, short GOP, keyframe on request.

### 5.3 Running as a service

| OS | Runs as | Notes |
|---|---|---|
| Windows | Windows service (LocalSystem), at boot | Works from session 0: C3 below. Installed from the setup page. |
| macOS | Background agent at login (`SMAppService`, registered by the app) | Camera/Microphone permission only exists in a user session, so not a boot-time daemon. A headless Mac needs auto-login. |
| Linux | systemd system service | Service user in the `video` and `audio` groups. |

#### Windows service: what C3 found (October 2026, DANIHTPC)

- **Video from session 0 works** on both cards: UGREEN 25773 (Media
  Foundation, 1920×1080 MJPEG at 30) and AVerMedia GC573 (DirectShow,
  1920×1080 NV12 at 60), same rates as from a normal window. No fallback to
  a login-time process was needed, so the service stays the design.
- **Audio from session 0 opens and delivers** on both (WASAPI for the
  UGREEN, the GC573's DirectShow pin): 48 kHz stereo, 5.0 s of audio per
  5 s. Windows feeds *silence* rather than an error when microphone privacy
  blocks an app, so the content check matters; device-wide microphone
  access (the setting that applies to LocalSystem) is on. Real sound from
  the service is confirmed (15 October): `misterkvms-cli stream
  mkvm://danihtpc/MiSTer-Pi` reported `audio_peak` 9762 from the GC573
  (0 = silence). The test pattern's beep (peak 12000) shows the server →
  viewer path carries audio.
- **Power throttling:** Windows treats a windowless process as background
  (EcoQoS); the GC573's JPEG encoding fell from 57 to 26 fps in the
  service. The server opts out (`SetProcessInformation`,
  `ProcessPowerThrottling`).
- **Encoding raw cards:** one encoder thread delivered 57.4 fps from the
  GC573 (an encode averages ~11 ms but sometimes runs past 16.7 ms, and the
  next frame replaced the waiting one, uncounted). Three encoder threads
  per stream, frames delivered in order: **60.0 fps** (1200/1200 frames in
  20 s), frame age 25 → 18 ms. Lost frames are now counted (`skipped`).
  Native-size pictures of raw cards are then encoded straight from the
  card's YUV (BT.709 limited → JPEG BT.601 full range in one step), keeping
  its 4:2:0 / 4:2:2 chroma, since finer chroma than the card sent adds bytes,
  not detail: 1080p encode 28.7 → 15.2 ms, 25% smaller
  (`codec/examples/jpeg_bench`); the GC573 stream went 48 → 36 Mbit/s,
  frame age 18 → 15 ms. RGB cards and scaled pictures stay 4:4:4.
- **How it's set up:** double-click `misterkvms-capture.exe` (opens the
  page; a second copy just opens the running one's page). "Run in the
  background" → *Install & start service*: a UAC prompt runs
  `misterkvms-capture service install`, which copies the program to Program
  Files, registers the service
  (`serve --service --config <dir>`, automatic start, restart on failure),
  adds firewall rules (TCP 5970, TCP 5971, UDP 5353, for that program), and
  lets Users edit the config folder (the service writes it as SYSTEM). The
  window's copy then exits; the service retries the ports until it has
  them; the page reconnects. From the service, Restart/Stop/Uninstall run
  in a helper process (already LocalSystem, no prompt). The service logs to
  `capture.log` in the config folder. CLI: `misterkvms-capture service
  install|uninstall|start|stop|restart|status`.
- **Install copies the program to `%ProgramFiles%\MiSTerKVMS\Capture`** and
  the service runs it from there, so the download can go. Double-clicking a
  newer download and pressing Start (or Install) replaces the installed copy.
  Uninstall removes it; the config folder stays.
- **Updates:** each release also carries the capture server on its own
  (`misterkvms-capture-<version>-win-x64.exe`; CI stamps the version into
  the build through `MISTERKVMS_VERSION`). *Check for updates* on the page
  asks GitHub for the latest release; *Update* (service only) downloads the
  exe into the Program Files folder (only administrators can write there),
  checks it against the SHA-256 GitHub lists for the asset, and reinstalls
  the service from it, no prompt needed. Double-clicking a newer download
  while an older service runs updates the service from it (one UAC prompt).

### 5.4 Setup page and tray

- A web page served by the server on **TCP 5971**, opened from a tray /
  menu-bar icon ("MiSTerKVMS Capture"). Same page on every OS, and from
  any browser for headless boxes.
- **Devices:** every card with a live preview (MJPEG in the browser), audio
  meter, formats, and "in use by another app". This is where users check
  the card works before any networking is involved.
- **Streams:** add / rename / remove; pick card, audio, format; MiSTer
  address with a "test connection" to mrext.
- **Status:** viewers per stream (name, quality, who has control), fps,
  drops, encoder; logs; restart.
- **Access:** viewing the page (status, previews) works from the capture PC
  only by default; "Allow from other computers" turns on LAN access. An
  optional password protects the configuration (changing streams and
  settings), wherever the page is opened from. Both live in `capture.ini`
  (§5.6): `[setup] allow_lan = false` and `password =`. Forgot the password?
  Delete the line in `capture.ini` and restart. LAN access and the service
  can only be changed from the capture PC itself, except *Update*, which
  works from any computer allowed to open the page (it can only install
  the latest release, checked against GitHub's SHA-256).
- **Test pattern:** "Show a test pattern" (`[setup] test_pattern`) adds a
  card with moving rainbow bars and a beep every second (its own card
  audio), to try streams, viewers, sound and recording without a MiSTer.
- The tray icon only starts/stops/restarts the service and opens the page.

### 5.5 Ports

| Port | Use |
|---|---|
| TCP 5970 | MKVM streams (all streams share it; the path picks one) |
| TCP 5971 | Setup page and previews |
| UDP 5353 | mDNS discovery (standard) |
| UDP 5970 | Later: QUIC transport |

The installer adds the firewall rules.

### 5.6 Installation

- **Windows:** one installer: program files, the service, firewall rules,
  the tray app at login; opens the setup page at the end. Uninstall removes
  the service and rules; config stays.
- **macOS:** one signed, notarised app, "MiSTerKVMS Capture.app". First
  launch asks for Camera and Microphone, registers the background agent and
  opens the setup page; the menu-bar icon stays.
- **Linux:** `.deb` and `.rpm` with the systemd unit and group setup; a
  tarball with `install.sh` for others.
- Config: `capture.json` in the config folder, written by the setup page
  (streams). Next to it `capture.ini` holds the few things a user may edit
  by hand: ports, `allow_lan` and the setup password.
  The CLI (`misterkvms-capture devices | add | serve | install-service`)
  stays for scripting.

## 6. Quality per viewer

- A **Quality** setting per host in the client: **Auto**, **Best** (native,
  MJPEG), **Balanced** (1080p, H.264), **Low** (720p, H.264). It sits in the
  host editor and in the View menu for quick switching; the change is a new
  `subscribe`, no reconnect.
- Auto starts at Best and steps down when drops or frame age rise, and tries
  stepping back up after a quiet minute.
- The server advertises the sizes it offers (native, 1080p, 720p, integer
  fractions of native) and runs one pipeline per distinct (size, codec);
  viewers asking for the same share it. Native MJPEG costs nothing. Integer
  ratios use an exact box filter (crisp pixel art), others an area filter.
  At most 3 pipelines per server by default; beyond that a viewer gets the
  nearest running one.

## 7. Several viewers, one in control

Any number of viewers can watch a stream. One of them at a time is **in
control**: its input reaches the MiSTer. The server holds the token and
the one mrext connection per stream, so this is enforced, not advisory.

Rules (in `mkvm`, tested as a state machine):

1. **Free token:** the first viewer to send input (or click the video) gets
   control at once.
2. **Take control always works,** immediately, with no prompt. The point is
   only that two machines never type at the same time. The previous holder
   sees "*Name* took control" and has the same **Take control** button to
   take it back.
3. **Handover safety:** when control moves or the holder disconnects, the
   server sends release-all to the MiSTer first (no stuck keys or buttons),
   and the previous holder's client leaves capture mode.
4. Input from a viewer without control is dropped by the server.
5. Disconnecting frees the token. Leaving capture mode doesn't.

In the client:

- A small indicator in the toolbar: "You have control" or "Watching —
  *Name* has control" with **Take control**.
- Without control, clicking the video asks to take control instead of
  entering capture mode; capture mode starts once control arrives.
- Viewer names default to the computer's name, editable in settings.

Local capture (§3) talks to mrext directly and has no token.

## 8. Client side

### 8.1 Sources

- `HostProfile` gets a source:
  - `{ kind: "capture", stream: "mkvm://pc:5970/MiSTer-SS1" }`: the MiSTer
    address comes from the stream, so it doesn't need entering;
  - `{ kind: "local", video_device, audio_device, mister_host }`;
  - saved NDI hosts from before R5 become capture hosts: when one is
    opened, the app finds the stream that shows the same MiSTer address and
    saves it.
- Decoding happens in the receive thread into the existing frame slot, so
  the renderer, auto-crop, predictive cursor, recording and stats stay as
  they are.
- `misterkvms-cli` screenshot/record/frames work with every source.

### 8.2 Networking

On the LAN there's nothing to set up: streams show up by MiSTer name, and
picking one fills in everything. The client only connects out, so there
are no firewall prompts on viewing machines. Typing `host[:port]/stream`
covers networks where mDNS doesn't pass (VLANs, some mesh Wi-Fi). Away from
home is not in v1 (§11).

### 8.3 UI changes (kept small)

Unchanged: sidebar, video view, capture mode and exit chord, cursor,
scaling, crop, recording. Changed:

- **Host editor:** the old "OBS PC + NDI source" fields became a source picker
  (streams found on the network / capture card on this computer / address)
  plus Quality.
- **Toolbar:** the control indicator (§7), shown only for server streams.
- **View menu:** Quality.
- **Stats overlay:** codec, size, viewers, server drops.
- **First run:** "Install MiSTerKVMS Capture on the PC with the capture
  card, or plug the card into this computer", replacing the OBS/NDI guide.

## 9. Licensing

- Everything here is **MIT** (D15).
- Dependencies stay permissive: OS encoders/decoders, `turbojpeg`,
  `zune-jpeg`, `openh264` (BSD; Cisco's binary covers the H.264 patent
  licence where needed), `quinn`, `cpal`, `mdns-sd`. No GPL ffmpeg builds.
- The OBS plugin (later) links libobs (GPL-2.0), so it will be
  GPL-2.0-or-later in its own folder; the apps are unaffected.
- With NDI removed, its runtime, SDK licence and redistribution rules go.

## 10. Milestones

| # | Milestone | Done when |
|---|---|---|
| C0 | Soak guard | `misterkvms-cli soak`; CI soak fails on growth |
| C1 | CLI renamed to `misterkvms-cli` | builds, docs, CI updated; `kvms` alias works |
| C2 | `mkvm` crate | protocol and control-token state machine with tests; fuzzed parser |
| C3 | `capture` + `codec` spike on Windows | UGREEN (MJPEG) and GC573 (raw) at 1080p60 with audio, from a normal process **and from a service** |
| C4 | Local capture in the apps | "Capture card on this computer" works on Windows and Mac; latency measured against NDI |
| C5 | `misterkvms-capture` on Windows, MJPEG | several streams, one mDNS entry each, mrext per stream, control token, service |
| C6 | Setup page + tray + Windows installer | previews, streams editor, status, optional setup password, firewall rules |
| C7 | `MkvmSource` + client UI | host editor source picker, control indicator, quality menu in all three apps |
| C8 | H.264 + per-viewer quality | hardware encode on Windows, decode in all three apps, Auto mode, shared pipelines |
| C9 | Server on macOS and Linux | background agent / systemd; signed app bundle; packages |
| C10 | Release (1.0); NDI removed | server and apps in releases; NDI code, SETUP-NDI and release notes removed; profiles migrate |
| Later | OBS plugin, Raspberry Pi capture box, QUIC + away from home, Mac App Store build | |

Status (15 October 2026): **C3** done on Windows (both cards, video and
audio from the service, real sound confirmed). **C5**: the service is in, set up from the page.
**C6**: the setup page does streams, previews, the service, LAN access,
the password and the test pattern; the tray icon and the installer are
still to do. Recording from streams and local cards works in the apps'
recorder and `misterkvms-cli record --stream/--local`. **C10** in part:
NDI is gone (R5): its code, SETUP-NDI and the NDI release notes are
removed, and saved NDI hosts migrate by MiSTer address (§8.1). The 1.0
release itself is still to come.

Rough size: C0–C4 about two weeks; C5–C7 about two weeks; C8–C10 about two
more. The biggest risks are C3 (cards, and capture from a Windows service)
and C8 (hardware codecs on three OSes).

## 11. Risks and open questions

- **Capture from a Windows service:** video and especially audio capture in
  session 0 may be blocked by privacy settings or driver behaviour; the
  fallback is a login-time background process (§5.3).
- **Latency:** MJPEG should match or beat the NDI it replaces (both
  intra-frame); input through the server adds one LAN hop (well under a
  millisecond). Measure end to end in C4/C5.
- **USB bandwidth:** two USB capture sticks on one controller may not both
  run 1080p60; the setup page should warn.
- **macOS capture box:** Camera permission, auto-login for headless use,
  and card driver support vary; the Windows PC stays the realistic box.
- **AVerMedia GC573:** on Windows it works through DirectShow (Media
  Foundation can't open it), video and its audio pin. Asking DirectShow to
  convert its frames crashes inside the card's filters, so frames are taken
  as the card sends them. Raw 1080p60 JPEG encoding takes 10–17 ms a frame:
  three encoder threads per stream (§5.3, C3).
- **Backends, not plugins:** new kinds of cards are new backends compiled
  into `capture` (vendor SDKs such as DeckLink would be loaded at run time,
  like NDI was). A plugin interface only if outside contributors need one.
- **Away from home and phones:** later. Options: QUIC through a forwarded
  port or Tailscale, a WebRTC output for browsers and phones.
- **Mac App Store (later):** the sandbox rules out Accessibility, so an App
  Store build loses ⌘Tab-style capture (PLANNING.md §2.2); everything else,
  including local capture, can stay.
