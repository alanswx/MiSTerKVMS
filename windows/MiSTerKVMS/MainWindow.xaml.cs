using System.Diagnostics;
using System.Globalization;
using System.Runtime.InteropServices;
using Microsoft.UI;
using Microsoft.UI.Input;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using MiSTerKVMS.Core;
using Windows.Foundation;
using Windows.System;

namespace MiSTerKVMS;

public sealed partial class MainWindow : Window
{
    private readonly KvmsCore _core = new();
    private readonly IntPtr _hwnd;
    private VideoSurface? _video;
    private List<HostProfile> _profiles = [];
    private ControllerState _state = new();
    /// <summary>Set while the host list is being refilled, so selection
    /// changes from that don't open or close hosts.</summary>
    private bool _updatingList;
    private readonly DispatcherTimer _hintTimer = new() { Interval = TimeSpan.FromSeconds(4) };

    public MainWindow()
    {
        InitializeComponent();
        _hwnd = WinRT.Interop.WindowNative.GetWindowHandle(this);
        SystemBackdrop = new Microsoft.UI.Xaml.Media.MicaBackdrop();
        AppWindow.Resize(new Windows.Graphics.SizeInt32(1280, 800));
        var icon = Path.Combine(AppContext.BaseDirectory, "Assets", "AppIcon.ico");
        if (File.Exists(icon)) AppWindow.SetIcon(icon);

        _core.Event += e => DispatcherQueue.TryEnqueue(() => OnCoreEvent(e));
        _hintTimer.Tick += (_, _) => { _hintTimer.Stop(); CaptureHint.Visibility = Visibility.Collapsed; };
        Root.Loaded += (_, _) =>
        {
            _video = new VideoSurface(_core, VideoHost);
            Root.XamlRoot.Changed += (_, _) => _video?.UpdateSize();
        };
        Activated += OnActivated;
        Closed += OnClosed;
        AddAccelerators();

        Log.Write($"settings: {_core.Call<string>("settingsPath")}");
        LoadProfiles();
        Render();
    }

    // MARK: Core events and rendering

    private void OnCoreEvent(string e)
    {
        switch (e)
        {
            case "changed":
                Render();
                break;
            case "profilesChanged":
                LoadProfiles();
                break;
            case "captureRequested":
                // Auto-capture when the connection comes up, but only if
                // the user is looking at this window.
                if (GetForegroundWindow() == _hwnd) StartCapture();
                break;
            case "releaseRequested":
                // The connection went down, or another viewer took control.
                Render();
                break;
        }
    }

    private void LoadProfiles()
    {
        _profiles = _core.Call<List<HostProfile>>("profiles");
        _updatingList = true;
        HostList.ItemsSource = _profiles;
        HostList.SelectedItem = _profiles.FirstOrDefault(p => p.Id == _state.ProfileId);
        _updatingList = false;
        NoHosts.Visibility = _profiles.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    private HostProfile? OpenProfile => _profiles.FirstOrDefault(p => p.Id == _state.ProfileId);

    private void Render()
    {
        _state = _core.Call<ControllerState>("state");
        var s = _state;
        var profile = OpenProfile;
        bool open = profile != null;
        bool connected = s.IsConnected;
        bool videoUp = s.VideoStats?.State == "connected";

        Title = profile == null ? "MiSTerKVMS"
            : $"{profile.Name} · {(s.CoreRunning.Length > 0 ? s.CoreRunning : profile.Subtitle)} — MiSTerKVMS";

        // Toolbar
        Toolbar.IsEnabled = open;
        MenuButton.IsEnabled = connected;
        CaptureButton.IsEnabled = connected || s.Capturing;
        CaptureButton.Label = s.Capturing ? "Release" : "Capture";
        CaptureIcon.Glyph = s.Capturing ? "" : "";
        ToolTipService.SetToolTip(CaptureButton, s.Capturing
            ? "Release keyboard and mouse (Right Win + Right Alt + Esc)"
            : "Send keyboard and mouse to MiSTer (or click the screen)");
        FitButton.IsChecked = s.Display.ScaleMode == ScaleMode.Fit;
        IntegerButton.IsChecked = s.Display.ScaleMode == ScaleMode.Integer;
        NativeButton.IsChecked = s.Display.ScaleMode == ScaleMode.Native;
        SmoothButton.IsChecked = s.Display.Smooth;
        AutoCropButton.IsChecked = s.Display.AutoCrop;
        PredictiveButton.IsChecked = s.Display.PredictiveCursor;
        ZoomButton.Label = $"{Math.Round(s.Display.Zoom * 100)}%";
        bool recording = s.RecordingSeconds != null;
        RecordButton.Label = recording ? "Stop" : "Record";
        RecordIcon.Glyph = recording ? "" : "";
        RecordIcon.Foreground = recording ? new SolidColorBrush(Colors.Red) : null;
        RecordButton.IsEnabled = recording || videoUp;
        MuteButton.IsChecked = s.Muted;
        MuteIcon.Glyph = s.Muted ? "" : "";
        PinStatsButton.IsChecked = s.StatsPinned;
        bool stream = profile?.SourceKind == "capture";
        QualityButton.Visibility = stream ? Visibility.Visible : Visibility.Collapsed;
        if (profile != null)
        {
            QualityAuto.IsChecked = profile.Quality is "auto" or "";
            QualityBest.IsChecked = profile.Quality == "best";
            QualityBalanced.IsChecked = profile.Quality == "balanced";
            QualityLow.IsChecked = profile.Quality == "low";
        }
        RenderControl(s);
        ShowRecordingButton.IsEnabled = s.LastRecording != null;

        RenderPlaceholder(profile, s, videoUp);
        RenderStats(s);
        RenderStatusBar(s);
    }

    private void RenderPlaceholder(HostProfile? profile, ControllerState s, bool videoUp)
    {
        Placeholder.Visibility = videoUp ? Visibility.Collapsed : Visibility.Visible;
        PlaceholderHint.Visibility = s.IsConnected && !s.Capturing ? Visibility.Visible : Visibility.Collapsed;
        if (profile == null)
        {
            PlaceholderIcon.Glyph = "";
            PlaceholderTitle.Text = _profiles.Count == 0 ? "Add a host to get started" : "Select a host";
            PlaceholderDetail.Text = _profiles.Count == 0 ? "" : "Choose a MiSTer on the left to connect.";
            return;
        }
        PlaceholderIcon.Glyph = "";
        if (profile.SourceKind == "capture")
        {
            PlaceholderTitle.Text = profile.StreamName.Length > 0 ? profile.StreamName : "No stream chosen";
            // Without a stream the core looks for the MiSTer's one and says so.
            PlaceholderDetail.Text = profile.StreamName.Length == 0
                ? s.Connection.Reason ?? "Edit the host and choose a MiSTerKVMS Capture stream."
                : s.Stream is { Connected: false } st && st.Error.Length > 0 ? st.Error
                : s.VideoStats?.State switch
                {
                    null or "connecting" => "Connecting to the capture server…",
                    "no signal" => "The capture card has no picture. Is the MiSTer on?",
                    var other => other,
                };
            return;
        }
        PlaceholderTitle.Text = "Capture card on this PC";
        PlaceholderDetail.Text = s.VideoError ?? s.VideoStats?.State switch
        {
            null or "opening" or "connecting" => "Opening the capture card…",
            "no video" => "The capture card has no picture. Is the MiSTer on?",
            var other => other,
        };
    }

    /// <summary>Capture server streams: who controls the MiSTer.</summary>
    private void RenderControl(ControllerState s)
    {
        ControlBanner.Visibility = s.SomeoneElseInControl ? Visibility.Visible : Visibility.Collapsed;
        if (s.Stream is not { } st)
        {
            ControlText.Text = "";
            return;
        }
        ControlBannerText.Text = $"{st.Holder} has control. Taking it releases their keyboard and mouse.";
        ControlText.Text = !st.Connected ? ""
            : st.HaveControl ? "You have control"
            : st.Holder.Length > 0 ? $"Watching · {st.Holder} has control"
            : "Watching · click the picture to take control";
    }

    private void TakeControl_Click(object sender, RoutedEventArgs e)
    {
        _core.Call("takeControl");
        StartCapture(force: true);
    }

    private void Quality_Click(object sender, RoutedEventArgs e) =>
        _core.Call("setQuality", new { quality = (string)((FrameworkElement)sender).Tag });

    private async void ViewerName_Click(object sender, RoutedEventArgs e)
    {
        var box = new TextBox { Text = _core.Call<string>("viewerName"), Header = "Other people watching a stream see this name" };
        var dialog = new ContentDialog
        {
            XamlRoot = Root.XamlRoot,
            Title = "Viewer name",
            Content = box,
            PrimaryButtonText = "Save",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
        };
        if (await dialog.ShowAsync() == ContentDialogResult.Primary) _core.Call("setViewerName", new { name = box.Text });
    }

    private void RenderStats(ControllerState s)
    {
        StatsPanel.Visibility = s.StatsVisible && OpenProfile != null ? Visibility.Visible : Visibility.Collapsed;
        StatsPinIcon.Glyph = s.StatsPinned ? "" : "";
        ToolTipService.SetToolTip(StatsPin, s.StatsPinned ? "Unpin: hide automatically (Ctrl+Shift+I)" : "Pin: always show (Ctrl+Shift+I)");
        if (StatsPanel.Visibility != Visibility.Visible) return;
        var c = CultureInfo.InvariantCulture;
        var lines = new List<string> { "INPUT" };
        if (s.InputStats is { } i)
        {
            lines.Add($"link   {(i.Connected ? "up" : "down")}");
            lines.Add(string.Format(c, "msg/s  {0:F0}", i.MessagesPerSec));
            lines.Add($"sent   {i.MessagesSent}");
            lines.Add($"queue  {i.Queued}");
        }
        if (s.Stream is { } st)
        {
            lines.Add("");
            lines.Add("STREAM");
            lines.Add($"server {(st.Connected ? "connected" : st.Error)}");
            lines.Add(string.Format(c, "rtt    {0:F1} ms", st.RoundTripMs));
            lines.Add($"mister {(st.MisterConnected ? st.Mister : "not connected")}");
            lines.Add($"ctrl   {(st.HaveControl ? "you" : st.Holder.Length > 0 ? st.Holder : "nobody")}");
        }
        lines.Add("");
        lines.Add("VIDEO");
        if (s.VideoStats is { } v)
        {
            lines.Add($"state  {v.State}");
            lines.Add($"src    {v.SourceWidth}×{v.SourceHeight}");
            lines.Add($"crop   {v.CropWidth}×{v.CropHeight}");
            lines.Add(string.Format(c, "fps    {0:F1} in / {1:F1} drawn", v.ReceivedFps, v.RenderedFps));
            lines.Add($"drop   {v.DroppedFrames} dropped / {v.SkippedFrames} skipped");
            lines.Add($"age    {AgeText(v.FrameAgeMs)}");
            lines.Add(string.Format(c, "audio  {0:F0} ms buf · {1} underruns", v.AudioBufferMs, v.AudioUnderruns));
            if (s.Display.PredictiveCursor)
            {
                lines.Add(string.Format(c, "cursor {0} · err {1:F0} px", v.CursorLocked ? "tracked" : "searching", v.CursorErrorPx));
                lines.Add(string.Format(c, "model  lag {0:F0} ms · ×{1:F2} · {2} fits/min", v.CursorLagMs, v.CursorGain, v.CursorFitSamples));
            }
        }
        else
        {
            lines.Add("off");
        }
        StatsText.Text = string.Join("\n", lines);
    }

    /// <summary>Capture-to-screen frame age (clock-corrected for streams);
    /// over a second almost always means the clocks couldn't be matched.</summary>
    private static string AgeText(double ms) => ms switch
    {
        < 0 => "n/a",
        > 1000 => string.Format(CultureInfo.InvariantCulture, "clocks differ (~{0:F1} s)", ms / 1000),
        _ => string.Format(CultureInfo.InvariantCulture, "≈{0:F0} ms capture→screen", ms),
    };

    private void RenderStatusBar(ControllerState s)
    {
        (StatusDot.Fill, StatusText.Text) = s.Connection.State switch
        {
            "connected" => (new SolidColorBrush(Colors.LimeGreen),
                s.CoreRunning.Length == 0 ? "Connected · MiSTer menu" : $"Connected · {s.CoreRunning}"),
            "connecting" => (new SolidColorBrush(Colors.Gold), "Connecting…"),
            _ => (new SolidColorBrush(Colors.Red), $"Disconnected: {s.Connection.Reason}"),
        };
        if (s.RecordingSeconds is { } secs)
        {
            RecordingDot.Visibility = Visibility.Visible;
            RecordingText.Text = TimeSpan.FromSeconds(secs).ToString(secs >= 3600 ? @"h\:mm\:ss" : @"m\:ss");
            RecordingText.Foreground = new SolidColorBrush(Colors.Red);
            LastRecordingLink.Visibility = Visibility.Collapsed;
        }
        else
        {
            RecordingDot.Visibility = Visibility.Collapsed;
            RecordingText.Text = s.RecordingError != null ? $"Recording failed: {s.RecordingError}" : "";
            LastRecordingLink.Visibility = s.LastRecording != null && s.RecordingError == null ? Visibility.Visible : Visibility.Collapsed;
            if (s.LastRecording != null) LastRecordingLink.Content = $"Saved {Path.GetFileName(s.LastRecording)} · Show in Explorer";
        }
        GameText.Text = s.GameRunning;
        CaptureText.Text = s.Capturing ? "Capturing — Right Win + Right Alt + Esc to release" : "F12 = MiSTer menu · not capturing";
    }

    // MARK: Hosts

    private void HostList_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_updatingList) return;
        if (HostList.SelectedItem is HostProfile p) _core.Call("open", new { id = p.Id });
        else _core.Call("close");
    }

    private async void AddHost_Click(object sender, RoutedEventArgs e) =>
        await EditHost(_core.Call<HostProfile>("newProfile"), isNew: true);

    private async void EditHost_Click(object sender, RoutedEventArgs e)
    {
        if (OpenProfile is { } p) await EditHost(p.Clone(), isNew: false);
    }

    private async void EditHostMenu_Click(object sender, RoutedEventArgs e)
    {
        if (_profiles.FirstOrDefault(p => p.Id == (string)((FrameworkElement)sender).Tag) is { } p)
            await EditHost(p.Clone(), isNew: false);
    }

    private async void DeleteHostMenu_Click(object sender, RoutedEventArgs e)
    {
        var id = (string)((FrameworkElement)sender).Tag;
        var profile = _profiles.FirstOrDefault(p => p.Id == id);
        if (profile == null) return;
        var confirm = new ContentDialog
        {
            XamlRoot = Root.XamlRoot,
            Title = $"Delete “{profile.Name}”?",
            Content = "The host's settings are removed. This can't be undone.",
            PrimaryButtonText = "Delete",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
        };
        if (await confirm.ShowAsync() == ContentDialogResult.Primary) _core.Call("deleteProfile", new { id });
    }

    private async Task EditHost(HostProfile profile, bool isNew)
    {
        StopCapture();
        var dialog = new HostEditorDialog(_core, profile) { XamlRoot = Root.XamlRoot };
        if (await dialog.ShowAsync() != ContentDialogResult.Primary) return;
        _core.Call("saveProfile", new { profile = dialog.Profile });
        if (isNew)
        {
            _core.Call("open", new { id = dialog.Profile.Id });
            LoadProfiles();
        }
    }

    // MARK: Capture

    /// <summary>Capture keyboard and mouse. On a stream someone else
    /// controls, only the Take control button (force) does.</summary>
    private void StartCapture(bool force = false)
    {
        if (!_state.IsConnected || _state.Capturing) return;
        if (_state.SomeoneElseInControl && !force) return;
        // The video area in screen pixels; the hidden pointer sits at its centre.
        double scale = Root.XamlRoot.RasterizationScale;
        Point origin = VideoArea.TransformToVisual(null).TransformPoint(new Point(0, 0));
        var p = new POINT { X = (int)(origin.X * scale), Y = (int)(origin.Y * scale) };
        ClientToScreen(_hwnd, ref p);
        try
        {
            _core.Call("startCapture", new
            {
                window = (long)_hwnd,
                x = p.X,
                y = p.Y,
                width = (int)(VideoArea.ActualWidth * scale),
                height = (int)(VideoArea.ActualHeight * scale),
            });
            CaptureHint.Visibility = Visibility.Visible;
            _hintTimer.Stop();
            _hintTimer.Start();
        }
        catch (CoreException e)
        {
            Log.Write($"capture: {e.Message}");
        }
    }

    private void StopCapture()
    {
        if (_state.Capturing) _core.Call("stopCapture");
    }

    private void Capture_Click(object sender, RoutedEventArgs e)
    {
        if (_state.Capturing) StopCapture();
        else StartCapture();
    }

    private void Video_PointerPressed(object sender, PointerRoutedEventArgs e)
    {
        if (e.GetCurrentPoint(VideoArea).Properties.IsLeftButtonPressed) StartCapture();
    }

    /// <summary>Not capturing: Ctrl+wheel zooms, the wheel pans when zoomed in.</summary>
    private void Video_PointerWheelChanged(object sender, PointerRoutedEventArgs e)
    {
        var props = e.GetCurrentPoint(VideoArea).Properties;
        double notches = props.MouseWheelDelta / 120.0;
        if (e.KeyModifiers.HasFlag(VirtualKeyModifiers.Control))
        {
            _core.Call("zoomBy", new { factor = Math.Pow(1.25, notches) });
        }
        else if (_state.Display.Zoom > 1)
        {
            double step = 0.05 * notches / _state.Display.Zoom;
            bool horizontal = props.IsHorizontalMouseWheel || e.KeyModifiers.HasFlag(VirtualKeyModifiers.Shift);
            _core.Call("panBy", horizontal ? new { dx = -step, dy = 0.0 } : new { dx = 0.0, dy = step });
        }
        e.Handled = true;
    }

    private void OnActivated(object sender, WindowActivatedEventArgs args)
    {
        // Release everything when another window takes over.
        if (args.WindowActivationState == WindowActivationState.Deactivated) StopCapture();
    }

    private void OnClosed(object sender, WindowEventArgs args)
    {
        StopCapture();
        _core.Call("close");
        _video?.Dispose();
        _video = null;
        _core.Dispose();
    }

    // MARK: Toolbar

    private void Menu_Click(object sender, RoutedEventArgs e) => _core.Call("openMenu");

    private void CtrlAltDel_Click(object sender, RoutedEventArgs e) => _core.Call("ctrlAltDel");

    private void SetDisplay(Action<DisplaySettings> change)
    {
        var d = _state.Display.Clone();
        change(d);
        _core.Call("setDisplay", new { display = d });
    }

    private void Scale_Click(object sender, RoutedEventArgs e) =>
        SetDisplay(d => d.ScaleMode = Enum.Parse<ScaleMode>((string)((FrameworkElement)sender).Tag));

    private void Smooth_Click(object sender, RoutedEventArgs e) => SetDisplay(d => d.Smooth = !d.Smooth);
    private void AutoCrop_Click(object sender, RoutedEventArgs e) => SetDisplay(d => d.AutoCrop = !d.AutoCrop);
    private void Predictive_Click(object sender, RoutedEventArgs e) => SetDisplay(d => d.PredictiveCursor = !d.PredictiveCursor);
    private void ZoomIn_Click(object sender, RoutedEventArgs e) => _core.Call("zoomBy", new { factor = 1.25 });
    private void ZoomOut_Click(object sender, RoutedEventArgs e) => _core.Call("zoomBy", new { factor = 1 / 1.25 });
    private void ResetZoom_Click(object sender, RoutedEventArgs e) => _core.Call("resetZoom");
    private void Mute_Click(object sender, RoutedEventArgs e) => _core.Call("setMuted", new { muted = !_state.Muted });
    private void ShowStats_Click(object sender, RoutedEventArgs e) => _core.Call("flashStats");
    private void PinStats_Click(object sender, RoutedEventArgs e) => TogglePin();
    private void StatsPin_Click(object sender, RoutedEventArgs e) => TogglePin();
    private void TogglePin() => _core.Call("setStatsPinned", new { pinned = !_state.StatsPinned });

    private async void Record_Click(object sender, RoutedEventArgs e) => await ToggleRecording();

    private async Task ToggleRecording()
    {
        if (_state.RecordingSeconds != null)
        {
            _core.Call("stopRecording");
            return;
        }
        if (OpenProfile is not { } profile || _state.VideoStats?.State != "connected") return;
        var dir = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.MyVideos), "MiSTerKVMS");
        var name = string.Concat(profile.Name.Select(c => Path.GetInvalidFileNameChars().Contains(c) ? '_' : c));
        var path = Path.Combine(dir, $"{name} {DateTime.Now:yyyy-MM-dd HH.mm.ss}.mp4");
        try
        {
            _core.Call("startRecording", new { path });
        }
        catch (CoreException ex)
        {
            await new ContentDialog
            {
                XamlRoot = Root.XamlRoot,
                Title = "Can't record",
                Content = ex.Message,
                CloseButtonText = "OK",
            }.ShowAsync();
        }
    }

    private void ShowRecording_Click(object sender, RoutedEventArgs e)
    {
        if (_state.LastRecording is { } path && File.Exists(path))
            Process.Start("explorer.exe", $"/select,\"{path}\"");
    }

    // MARK: App menu

    private async void Export_Click(object sender, RoutedEventArgs e)
    {
        var picker = new Windows.Storage.Pickers.FileSavePicker { SuggestedFileName = "MiSTerKVMS hosts" };
        picker.FileTypeChoices.Add("JSON", [".json"]);
        WinRT.Interop.InitializeWithWindow.Initialize(picker, _hwnd);
        if (await picker.PickSaveFileAsync() is { } file)
            await File.WriteAllTextAsync(file.Path, _core.Call<string>("exportProfiles"));
    }

    private async void Import_Click(object sender, RoutedEventArgs e)
    {
        var picker = new Windows.Storage.Pickers.FileOpenPicker();
        picker.FileTypeFilter.Add(".json");
        WinRT.Interop.InitializeWithWindow.Initialize(picker, _hwnd);
        if (await picker.PickSingleFileAsync() is not { } file) return;
        string message;
        try
        {
            int count = _core.Call<int>("importProfiles", new { json = await File.ReadAllTextAsync(file.Path) });
            message = count == 1 ? "Imported 1 host." : $"Imported {count} hosts.";
        }
        catch (CoreException ex)
        {
            message = $"That file couldn't be read: {ex.Message}";
        }
        await new ContentDialog { XamlRoot = Root.XamlRoot, Title = "Import hosts", Content = message, CloseButtonText = "OK" }
            .ShowAsync();
    }

    private void OpenLog_Click(object sender, RoutedEventArgs e)
    {
        if (File.Exists(Log.FilePath)) Process.Start(new ProcessStartInfo(Log.FilePath) { UseShellExecute = true });
    }

    private async void About_Click(object sender, RoutedEventArgs e) =>
        await new AboutDialog { XamlRoot = Root.XamlRoot }.ShowAsync();

    // MARK: Keyboard shortcuts (while not capturing; capture swallows keys)

    private void AddAccelerators()
    {
        void Add(VirtualKey key, VirtualKeyModifiers mods, Action action)
        {
            var a = new KeyboardAccelerator { Key = key, Modifiers = mods };
            a.Invoked += (_, args) => { action(); args.Handled = true; };
            Root.KeyboardAccelerators.Add(a);
        }
        const VirtualKeyModifiers Ctrl = VirtualKeyModifiers.Control;
        const VirtualKeyModifiers CtrlShift = VirtualKeyModifiers.Control | VirtualKeyModifiers.Shift;
        Add(VirtualKey.K, CtrlShift, () => StartCapture());
        Add(VirtualKey.I, CtrlShift, TogglePin);
        Add(VirtualKey.R, CtrlShift, () => _ = ToggleRecording());
        Add(VirtualKey.M, CtrlShift, () => _core.Call("setMuted", new { muted = !_state.Muted }));
        Add((VirtualKey)187, Ctrl, () => _core.Call("zoomBy", new { factor = 1.25 }));     // =
        Add(VirtualKey.Add, Ctrl, () => _core.Call("zoomBy", new { factor = 1.25 }));
        Add((VirtualKey)189, Ctrl, () => _core.Call("zoomBy", new { factor = 1 / 1.25 })); // -
        Add(VirtualKey.Subtract, Ctrl, () => _core.Call("zoomBy", new { factor = 1 / 1.25 }));
        Add(VirtualKey.Number0, Ctrl, () => _core.Call("resetZoom"));
        Root.KeyboardAcceleratorPlacementMode = KeyboardAcceleratorPlacementMode.Hidden;
    }

    // MARK: Win32

    [StructLayout(LayoutKind.Sequential)]
    private struct POINT
    {
        public int X, Y;
    }

    [DllImport("user32.dll")]
    private static extern bool ClientToScreen(IntPtr hwnd, ref POINT point);

    [DllImport("user32.dll")]
    private static extern IntPtr GetForegroundWindow();
}
