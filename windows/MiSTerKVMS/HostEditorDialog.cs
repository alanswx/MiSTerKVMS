using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Documents;
using MiSTerKVMS.Core;

namespace MiSTerKVMS;

/// <summary>Add or edit a host: where its video comes from (a capture server
/// stream, a capture card on this PC, or OBS via NDI), the MiSTer, mouse
/// options.</summary>
public sealed class HostEditorDialog : ContentDialog
{
    private readonly KvmsCore _core;
    private readonly TextBox _name = new() { Header = "Name" };

    private static readonly (string Kind, string Label)[] Sources =
    [
        ("capture", "MiSTerKVMS Capture (a stream on the network)"),
        ("local", "A capture card on this PC"),
        ("ndi", "OBS via NDI (older setups)"),
    ];
    private readonly ComboBox _source = new() { Header = "Video & audio from", HorizontalAlignment = HorizontalAlignment.Stretch };

    // Capture server stream
    private readonly ComboBox _stream = new() { Header = "Stream", IsEditable = true, PlaceholderText = "Choose, or type mkvm://pc/stream", HorizontalAlignment = HorizontalAlignment.Stretch };
    private readonly Button _findStreams = new() { Content = "Find", VerticalAlignment = VerticalAlignment.Bottom };
    private readonly ProgressRing _searchingStreams = new() { Width = 16, Height = 16, IsActive = false, VerticalAlignment = VerticalAlignment.Bottom, Margin = new Thickness(0, 0, 0, 8) };
    private readonly TextBlock _streamResult = Caption();
    private readonly ComboBox _quality = new() { Header = "Quality", HorizontalAlignment = HorizontalAlignment.Stretch };
    private static readonly (string Value, string Label)[] Qualities =
    [
        ("auto", "Automatic"),
        ("best", "Best (full size, for wired networks)"),
        ("balanced", "Balanced (up to 1080p)"),
        ("low", "Low (up to 720p, for slow Wi-Fi)"),
    ];
    private List<FoundStream> _foundStreams = [];

    // Capture card on this PC
    private readonly ComboBox _card = new() { Header = "Capture card", PlaceholderText = "Choose a card", HorizontalAlignment = HorizontalAlignment.Stretch };
    private readonly ComboBox _audio = new() { Header = "Audio", HorizontalAlignment = HorizontalAlignment.Stretch };
    private readonly Button _findCards = new() { Content = "Refresh", VerticalAlignment = VerticalAlignment.Bottom };
    private readonly TextBlock _cardResult = Caption();
    private List<CaptureCard> _cards = [];

    // NDI
    private readonly TextBox _obsHost = new() { Header = "OBS PC (optional)", PlaceholderText = "IP or name, e.g. obs-pc.local" };
    private readonly ComboBox _ndiSource = new() { Header = "NDI source", IsEditable = true, PlaceholderText = "Choose or type", HorizontalAlignment = HorizontalAlignment.Stretch };
    private readonly Button _find = new() { Content = "Find", VerticalAlignment = VerticalAlignment.Bottom };
    private readonly ProgressRing _searching = new() { Width = 16, Height = 16, IsActive = false, VerticalAlignment = VerticalAlignment.Bottom, Margin = new Thickness(0, 0, 0, 8) };
    private readonly TextBlock _findResult = Caption();

    // MiSTer
    private readonly TextBox _misterHost = new() { Header = "MiSTer IP or hostname", PlaceholderText = "192.168.1.50" };
    private readonly NumberBox _misterPort = new() { Header = "mrext port", Minimum = 1, Maximum = 65535, SpinButtonPlacementMode = NumberBoxSpinButtonPlacementMode.Hidden };

    private readonly Slider _sensitivity = new() { Header = "Mouse sensitivity", Minimum = 0.1, Maximum = 4, StepFrequency = 0.05, TickFrequency = 0.5 };
    private readonly CheckBox _altClick = new() { Content = "Alt + click sends a middle click" };
    private readonly ToggleSwitch _rawMouse = new() { Header = "Raw mouse input", OnContent = "On", OffContent = "Off" };

    private readonly StackPanel _streamPanel = new() { Spacing = 12 };
    private readonly StackPanel _localPanel = new() { Spacing = 12 };
    private readonly StackPanel _ndiPanel = new() { Spacing = 12 };
    private readonly StackPanel _misterPanel = new() { Spacing = 12 };
    private readonly TextBlock _misterViaServer = Caption("Keyboard and mouse go through the capture server, which knows this MiSTer's address.");

    private int _searchId;
    private CancellationTokenSource? _debounce;

    public HostProfile Profile { get; }

    public HostEditorDialog(KvmsCore core, HostProfile profile)
    {
        _core = core;
        Profile = profile;
        bool isNew = profile.MisterHost.Length == 0 && profile.NdiSource.Length == 0 && profile.Stream.Length == 0 && profile.LocalVideo.Length == 0;
        Title = isNew ? "Add host" : "Edit host";
        PrimaryButtonText = "Save";
        CloseButtonText = "Cancel";
        DefaultButton = ContentDialogButton.Primary;

        _name.Text = profile.Name;
        _source.ItemsSource = Sources.Select(s => s.Label).ToList();
        // New hosts start on the capture server; old ones keep what they use.
        string kind = isNew ? "capture" : profile.SourceKind;
        _source.SelectedIndex = Array.FindIndex(Sources, s => s.Kind == kind);
        _stream.Text = profile.Stream;
        _quality.ItemsSource = Qualities.Select(q => q.Label).ToList();
        _quality.SelectedIndex = Math.Max(0, Array.FindIndex(Qualities, q => q.Value == profile.Quality));
        _misterHost.Text = profile.MisterHost;
        _misterPort.Value = profile.MisterPort;
        _obsHost.Text = profile.ObsHost;
        _ndiSource.Text = profile.NdiSource;
        _sensitivity.Value = profile.MouseSensitivity;
        _altClick.IsChecked = profile.AltClickIsMiddle;
        _rawMouse.IsOn = profile.RawMouse;

        _name.TextChanged += (_, _) => IsPrimaryButtonEnabled = _name.Text.Trim().Length > 0;
        _source.SelectionChanged += (_, _) => ShowSource();
        _find.Click += (_, _) => FindSources();
        _findStreams.Click += (_, _) => FindStreams();
        _findCards.Click += (_, _) => FindCards();
        _card.SelectionChanged += (_, _) =>
        {
            if (_card.SelectedItem is CaptureCard c && c.Audio.Length > 0) SelectAudio(c.Audio);
        };
        _obsHost.TextChanged += async (_, _) =>
        {
            // Search again once typing pauses.
            _debounce?.Cancel();
            _debounce = new CancellationTokenSource();
            try
            {
                await Task.Delay(800, _debounce.Token);
                FindSources();
            }
            catch (TaskCanceledException)
            {
            }
        };
        Opened += (_, _) => ShowSource(search: true);
        PrimaryButtonClick += (_, _) => Save();

        var ndiNote = Caption();
        ndiNote.Inlines.Add(new Run { Text = "Enter the OBS PC to list its sources directly; otherwise sources found on the local network are listed. Needs the NDI® runtime (" });
        var link = new Hyperlink { NavigateUri = new Uri("https://ndi.video") };
        link.Inlines.Add(new Run { Text = "ndi.video" });
        ndiNote.Inlines.Add(link);
        ndiNote.Inlines.Add(new Run { Text = ")." });

        _streamPanel.Children.Add(SearchRow(_stream, _searchingStreams, _findStreams));
        _streamPanel.Children.Add(_streamResult);
        _streamPanel.Children.Add(_quality);
        _streamPanel.Children.Add(Caption("Streams come from MiSTerKVMS Capture running on the PC with the capture card. Several people can watch; one at a time controls the MiSTer."));

        _localPanel.Children.Add(SearchRow(_card, null, _findCards));
        _localPanel.Children.Add(_audio);
        _localPanel.Children.Add(_cardResult);
        _localPanel.Children.Add(Caption("Only one program can use a capture card at a time: close OBS or MiSTerKVMS Capture if they have it open."));

        _ndiPanel.Children.Add(_obsHost);
        _ndiPanel.Children.Add(SearchRow(_ndiSource, _searching, _find));
        _ndiPanel.Children.Add(_findResult);
        _ndiPanel.Children.Add(ndiNote);

        _misterPanel.Children.Add(Section("MiSTer (keyboard & mouse via mrext Remote)"));
        _misterPanel.Children.Add(Row(_misterHost, _misterPort, 120));

        var rawNote = Caption("Sends your mouse's own movement without Windows pointer acceleration, so only the vintage OS accelerates it, like a mouse plugged straight in.");

        Content = new ScrollViewer
        {
            Content = new StackPanel
            {
                Spacing = 12,
                MinWidth = 460,
                Children =
                {
                    _name,
                    Section("Video & audio"),
                    _source,
                    _streamPanel,
                    _localPanel,
                    _ndiPanel,
                    _misterViaServer,
                    _misterPanel,
                    Section("Mouse"),
                    _sensitivity,
                    _altClick,
                    _rawMouse,
                    rawNote,
                },
            },
        };
        ShowSource();
    }

    private string Kind => Sources[Math.Max(0, _source.SelectedIndex)].Kind;

    private void ShowSource(bool search = false)
    {
        string kind = Kind;
        _streamPanel.Visibility = kind == "capture" ? Visibility.Visible : Visibility.Collapsed;
        _localPanel.Visibility = kind == "local" ? Visibility.Visible : Visibility.Collapsed;
        _ndiPanel.Visibility = kind == "ndi" ? Visibility.Visible : Visibility.Collapsed;
        _misterPanel.Visibility = kind == "capture" ? Visibility.Collapsed : Visibility.Visible;
        _misterViaServer.Visibility = kind == "capture" ? Visibility.Visible : Visibility.Collapsed;
        if (kind == "capture" && (search || _foundStreams.Count == 0)) FindStreams();
        if (kind == "local" && (search || _cards.Count == 0)) FindCards();
        if (kind == "ndi" && search) FindSources();
    }

    private static TextBlock Caption(string text = "") => new()
    {
        Text = text,
        TextWrapping = TextWrapping.Wrap,
        Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"],
    };

    private static TextBlock Section(string text) => new()
    {
        Text = text,
        Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"],
        Margin = new Thickness(0, 8, 0, 0),
    };

    private static Grid Row(FrameworkElement wide, FrameworkElement narrow, double narrowWidth)
    {
        var grid = new Grid { ColumnSpacing = 8 };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(narrowWidth) });
        grid.Children.Add(wide);
        Grid.SetColumn(narrow, 1);
        grid.Children.Add(narrow);
        return grid;
    }

    private static Grid SearchRow(FrameworkElement field, ProgressRing? ring, Button button)
    {
        var row = new Grid { ColumnSpacing = 8 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.Children.Add(field);
        if (ring != null)
        {
            Grid.SetColumn(ring, 1);
            row.Children.Add(ring);
        }
        Grid.SetColumn(button, 2);
        row.Children.Add(button);
        return row;
    }

    /// <summary>Capture server streams on the network (mDNS, a couple of
    /// seconds, off the UI thread).</summary>
    private async void FindStreams()
    {
        int id = ++_searchId;
        _searchingStreams.IsActive = true;
        _findStreams.IsEnabled = false;
        _streamResult.Text = "Looking for streams…";
        List<FoundStream> found = [];
        await Task.Run(() =>
        {
            try
            {
                found = _core.Call<List<FoundStream>>("findStreams", new { waitMs = 2500 });
            }
            catch (CoreException)
            {
            }
        });
        if (id != _searchId) return;
        _searchingStreams.IsActive = false;
        _findStreams.IsEnabled = true;
        _foundStreams = found;
        string typed = _stream.Text;
        _stream.ItemsSource = found;
        var current = found.FirstOrDefault(f => f.Address == typed || f.Name == Profile.StreamName);
        if (current != null) _stream.SelectedItem = current;
        else _stream.Text = typed;
        if (found.Count == 0)
            _streamResult.Text = "No streams found. Is MiSTerKVMS Capture running on the capture PC, with a stream set up? You can also type its address.";
        else
        {
            _streamResult.Text = found.Count == 1 ? "Found 1 stream." : $"Found {found.Count} streams.";
            if (typed.Length == 0 && found.Count == 1) _stream.SelectedItem = found[0];
        }
    }

    private async void FindCards()
    {
        _findCards.IsEnabled = false;
        _cardResult.Text = "Looking for capture cards…";
        List<CaptureCard> cards = [];
        List<AudioInput> audio = [];
        await Task.Run(() =>
        {
            try
            {
                cards = _core.Call<List<CaptureCard>>("captureCards");
                audio = _core.Call<List<AudioInput>>("audioInputs");
            }
            catch (CoreException)
            {
            }
        });
        _findCards.IsEnabled = true;
        _cards = cards;
        _card.ItemsSource = cards;
        _audio.ItemsSource = new List<AudioInput> { new() { Id = "", Name = "No audio" } }.Concat(audio).ToList();
        _card.SelectedItem = cards.FirstOrDefault(c => c.Id == Profile.LocalVideo) ?? (cards.Count == 1 ? cards[0] : null);
        SelectAudio(Profile.LocalVideo.Length > 0 ? Profile.LocalAudio : (_card.SelectedItem as CaptureCard)?.Audio ?? "");
        _cardResult.Text = cards.Count == 0 ? "No capture cards found on this PC." : "";
    }

    private void SelectAudio(string id)
    {
        if (_audio.ItemsSource is List<AudioInput> list)
            _audio.SelectedItem = list.FirstOrDefault(a => a.Id == id) ?? list[0];
    }

    /// <summary>Look for NDI sources (on the network and on the OBS PC, if
    /// given). Discovery blocks for a couple of seconds, so it runs off the UI
    /// thread; an older search finishing late is ignored.</summary>
    private async void FindSources()
    {
        int id = ++_searchId;
        string hosts = _obsHost.Text.Trim();
        _searching.IsActive = true;
        _find.IsEnabled = false;
        _findResult.Text = "Searching…";
        List<string>? found = null;
        string? error = null;
        await Task.Run(() =>
        {
            try
            {
                found = _core.Call<List<string>>("findSources", new { obsHosts = hosts, waitMs = 2000 });
            }
            catch (CoreException e)
            {
                error = e.Message;
            }
        });
        if (id != _searchId) return;
        _searching.IsActive = false;
        _find.IsEnabled = true;
        string typed = _ndiSource.Text;
        _ndiSource.ItemsSource = found ?? [];
        _ndiSource.Text = typed;
        if (error != null)
        {
            _findResult.Text = error;
        }
        else if (found!.Count == 0)
        {
            _findResult.Text = "No sources found.";
        }
        else
        {
            _findResult.Text = $"Found: {string.Join(", ", found)}";
            if (typed.Length == 0 && found.Count == 1) _ndiSource.Text = found[0];
        }
    }

    private void Save()
    {
        Profile.Name = _name.Text.Trim();
        Profile.Source = Kind;
        Profile.Stream = _stream.SelectedItem is FoundStream f ? f.Address : _stream.Text.Trim();
        Profile.Quality = Qualities[Math.Max(0, _quality.SelectedIndex)].Value;
        if (_card.SelectedItem is CaptureCard card) Profile.LocalVideo = card.Id;
        Profile.LocalAudio = (_audio.SelectedItem as AudioInput)?.Id ?? "";
        Profile.MisterHost = _misterHost.Text.Trim();
        Profile.MisterPort = double.IsNaN(_misterPort.Value) ? (ushort)8182 : (ushort)_misterPort.Value;
        Profile.ObsHost = _obsHost.Text.Trim();
        Profile.NdiSource = (_ndiSource.SelectedItem as string ?? _ndiSource.Text).Trim();
        Profile.MouseSensitivity = Math.Round(_sensitivity.Value, 2);
        Profile.AltClickIsMiddle = _altClick.IsChecked == true;
        Profile.RawMouse = _rawMouse.IsOn;
    }
}
