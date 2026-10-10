# OBS PC setup: NDI output for MiSTerKVMS

These steps get each MiSTer's capture source out of OBS as its own NDI stream, with video and audio, so the Mac app can receive it.

Tested target: Windows 10, OBS 31.1.1+.

## 1. Requirements

- **OBS Studio 31.1.1 or newer** (64-bit). Check with *Help → About*. Update if it's older.
- **NDI Runtime 6.3 or newer**. The DistroAV installer usually offers it. If OBS later says the NDI runtime is missing, install it from <https://ndi.video/tools/>.
- Wired Ethernet on both the OBS PC and the Mac is strongly recommended (a 1080p60 NDI stream is ~100–150 Mbps).

## 2. Install DistroAV (NDI plugin for OBS)

1. Close OBS.
2. Open **PowerShell** and run:
   ```powershell
   winget install --exact --id DistroAV.DistroAV
   ```
   Alternatively, download the Windows installer from the [DistroAV releases page](https://github.com/DistroAV/DistroAV/releases) or <https://distroav.org/download>.
3. Restart Windows if the NDI Runtime was installed.
4. Start OBS. You should now have **Tools → DistroAV NDI Settings**.

## 3. Windows network/firewall

NDI finds sources with mDNS (UDP 5353) and streams over TCP/UDP starting at port 5960.

1. *Settings → Network & Internet → Ethernet → (your network)*: set the profile to **Private**.
2. If Windows asks whether to allow OBS on networks the first time NDI starts, allow **Private** networks.
3. If the Mac can't see the source later, check *Windows Defender Firewall → Allow an app* and make sure **obs64.exe** is allowed on Private.

## 4. Configure the MiSTer capture source

For each MiSTer, starting with the HDMI-direct one:

1. Select the **Video Capture Device** source for that MiSTer and open **Properties**:
   - **Resolution/FPS Type**: Custom. **Resolution** = what the MiSTer outputs (e.g. 1920×1080), **FPS** = match output (e.g. 60).
   - **Buffering**: **Disable**. This removes extra frames of latency.
   - **Audio Output Mode**: **Capture audio only**. This keeps HDMI audio attached to the source so it goes into NDI.
   - If HDMI audio comes through a separate audio device, tick **Use custom audio device** and pick it.
2. Right-click the source → **Filters**. Use the **+** under the lower **Effect Filters** list (*not* the top *Audio/Video Filters* list, which only offers the audio-only NDI filter). Pick **DistroAV NDI® Output** (may also be named *Dedicated NDI® output*). It sends both video and the source's audio, so don't also add *NDI Output (Audio Only)*.
3. Give it a clear **NDI name** without spaces, e.g. `MISTER-1`. This is the name you'll enter in the app.
4. Leave other options at default. Close the filter window.

Repeat with a different name (`MISTER-2`, ...) for other MiSTers.

> You do **not** need to enable *Main Output* in DistroAV settings. The per-source filter is what we use, so each MiSTer has its own stream no matter what scene is on screen in OBS.

The NDI filter only sends while its source is active. If you see a black or missing stream, put the source in a scene (it can be hidden behind another source or in a scene that isn't on air). Then check Tools → DistroAV NDI Settings for an option about outputting inactive sources.

## 5. Verify from the Mac

1. On the Mac, install **NDI Tools for macOS** from <https://ndi.video/tools/>.
2. Open **NDI Video Monitor**. In its source list you should see `OBS-PC-NAME (MISTER-1)`.
3. Select it. You should see the MiSTer picture and hear audio.
4. Rough latency check: put a stopwatch or a fast-changing counter on the MiSTer, and take a phone photo showing both the TV/monitor and the Mac screen.

If it doesn't show up:
- Make sure the Mac and PC are on the same subnet/VLAN. mDNS doesn't cross subnets.
- Check the Windows firewall (step 3).
- In NDI Video Monitor or Access Manager, add the OBS PC's IP manually as an extra IP.

## 6. MiSTer side (optional, for sharper pixels)

Nothing is required here; the app works with whatever the MiSTer outputs. If you want the cleanest picture, give each computer core 1080p output with integer scaling in `MiSTer.ini` (on the SD card root). This is the same pattern already used for `[NeXT]` and `[SGIIndy]`. For example:

```ini
[MacPPC7300]
video_mode=8          ; 1920x1080@60
vscale_mode=1         ; integer scaling: 640x480 → 2x, 800x600/1024x768 → 1x, pixel-exact
```

Without a core section, MiSTer's default is 1280×720 with fit-to-height scaling, which blurs 640×480/800×600 and *downscales* 1024×768. See PLANNING.md §4.1. (The `.92` MiSTer's `[HP9K715]` section already does this.)

…and leave the scaler filter off (OSD → *Video processing → filters*). Check the [MiSTer.ini documentation](https://github.com/MiSTer-devel/Main_MiSTer/blob/master/MiSTer.ini) for the exact values for your setup.

## What to send back

When it works, note:
- The **NDI source name** as shown on the Mac (e.g. `DESKTOP-ABC (MISTER-1)`).
- The **OBS PC IP** and **MiSTer IP**.
- The resolution/fps the capture card receives.
