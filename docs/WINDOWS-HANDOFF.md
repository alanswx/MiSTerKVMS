# Windows handoff: capture server setup, service, end-to-end test

Paste the prompt below into Claude Code on the Windows capture PC
(DANIHTPC, 192.168.99.82), in the MiSTerKVMS checkout.

---

You're continuing work on MiSTerKVMS on the `native-protocol` branch. The
Mac side is done (see "Where things are"); this session finishes the
capture server on Windows so it can be set up entirely from its web page
and left running as a service, then tests it end to end with the real
cards and MiSTers. Read `docs/CAPTURE-SERVER.md` first (decisions R1–R13,
milestones C3–C10), then `git pull` and `git log --oneline -15`.

Rules: commit as you go with clear messages; don't push until I say so
(pushes to `main` publish a release). Don't change mrext, and never edit
the MiSTers' ini files. Don't change the protocol without updating the
`mkvm` tests: the Mac app speaks it too.

## Where things are

- **Mac (done, merged to main):** AVFoundation capture works (built-in
  and iPhone cameras: formats, BGRA frames, fast open/close). The macOS
  app has the new host editor (capture stream / capture card on this Mac /
  NDI), the "X has control" banner with Take Control, Quality in the View
  menu, Viewer name in Settings.
- **mDNS fix:** the server used to announce streams under the computer's
  own host name (`<host>.local`), which the OS's mDNS responder owns: on
  the Mac that renamed the computer to `<host>-2` and discovery broke a few
  minutes later. It now uses `<host>-mkvm.local`
  (`server/src/discovery.rs`). Windows 10/11 also runs an mDNS responder,
  so check this here too (below).
- **Releases:** `misterkvms-capture.exe` now ships in the Windows zip
  (`scripts/build-windows.ps1`) and inside the Mac app. CI builds Mac,
  Windows and Linux in parallel and publishes once all three succeed
  (`.github/workflows/release.yml`).
- **Setup page (port 5971, `server/src/setup.rs` + `setup.html`):**
  already adds, edits and removes streams (card, resolution, frame rate,
  audio, MiSTer address, test connection, live preview), and sets the
  setup password. Streams go live when saved. Settings live in
  `C:\ProgramData\MiSTerKVMS\Capture\capture.ini`; streams in
  `capture.json` there.

## What I want

I don't want a command-line workflow. Double-click
`misterkvms-capture.exe`, do everything in the browser (add streams one
at a time, configure them), then flip one switch so it keeps running as
a service. Concretely:

1. **Double-click opens the page.** Started with no arguments from
   Explorer (not from a terminal, not as a service), open
   http://localhost:5971 in the default browser once the server is
   listening. If another copy (or the service) already has the ports,
   just open the page and exit instead of failing.
2. **Run as a service, from the page.** A "Run in the background"
   section: status (running as service / running in this window / not
   installed), and buttons **Install & start service**, **Stop**,
   **Restart**, **Uninstall**. The service runs `misterkvms-capture serve`
   at boot (Windows service via the Service Control Manager; the
   `windows-service` crate is MIT/Apache). Installing needs admin: trigger
   a UAC prompt (`ShellExecute` with `runas` on
   `misterkvms-capture service install`), don't require starting the whole
   thing elevated. Add matching CLI subcommands
   (`service install|uninstall|start|stop|status`). When the console copy
   hands over to the service, the console copy exits and the page keeps
   working (same ports, same config folder).
3. **Spike C3 first:** before building the UI, confirm video **and
   audio** capture work from the service (session 0) on both cards: the
   UGREEN 25773 (Media Foundation, MJPEG) and the AVerMedia GC573
   (DirectShow, raw, with its audio pin). If audio or video fails in
   session 0, fall back to a per-user background process started at
   login (Task Scheduler "at logon" task, or the Run key) and make the
   page's switch do that instead. Write down what you found in
   `CAPTURE-SERVER.md`.
4. **Firewall:** installing the service adds inbound rules for TCP 5970
   and 5971 (and UDP 5353 if needed for mDNS); uninstalling removes them.
5. **Setup from other computers:** an "Allow this page from other
   computers" switch on the page (writes `allow_lan` in `capture.ini`),
   usable only from the PC itself. Keep the password rule (R10).

Keep the page's look as it is; add the new section in the same style.

## Test end to end

- Close OBS. Set up both streams **only through the page**:
  `MiSTer-SS1` → MiSTer 192.168.99.92, `MiSTer-Pi` → MiSTer 192.168.99.143,
  each with its card and audio. Use **test connection** for each MiSTer.
- Install the service from the page, reboot, check both streams come back
  without logging in (or at login, if C3 forced the fallback).
- mDNS: `misterkvms-cli streams` finds both, still after 10 minutes; the
  PC's own name hasn't changed (`hostname` before and after; Windows
  Event Viewer / `Resolve-DnsName danihtpc.local`).
- From the Windows app and the Mac app (192.168.99.159): video and
  audio, quality switching, **Take control** back and forth between the
  two (the other side must show "X has control" and stop sending input).
- `misterkvms-cli soak 10m --stream mkvm://127.0.0.1/MiSTer-SS1` and
  `soak 10m --local <card>` (with the service stopped): memory flat.

## Finish

Update `docs/CAPTURE-SERVER.md` (C3, C5, C6, the service decision, what
you measured), then report: what works, what doesn't, anything that needs
my decision.
