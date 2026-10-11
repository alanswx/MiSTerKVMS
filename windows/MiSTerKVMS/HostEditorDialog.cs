using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Documents;
using MiSTerKVMS.Core;

namespace MiSTerKVMS;

/// <summary>Add or edit a host: the MiSTer, its NDI source, mouse options.</summary>
public sealed class HostEditorDialog : ContentDialog
{
    private readonly KvmsCore _core;
    private readonly TextBox _name = new() { Header = "Name" };
    private readonly TextBox _misterHost = new() { Header = "MiSTer IP or hostname", PlaceholderText = "192.168.1.50" };
    private readonly NumberBox _misterPort = new() { Header = "mrext port", Minimum = 1, Maximum = 65535, SpinButtonPlacementMode = NumberBoxSpinButtonPlacementMode.Hidden };
    private readonly TextBox _obsHost = new() { Header = "OBS PC (optional)", PlaceholderText = "IP or name, e.g. obs-pc.local" };
    private readonly ComboBox _ndiSource = new() { Header = "NDI source", IsEditable = true, PlaceholderText = "Choose or type", HorizontalAlignment = HorizontalAlignment.Stretch };
    private readonly Button _find = new() { Content = "Find", VerticalAlignment = VerticalAlignment.Bottom };
    private readonly ProgressRing _searching = new() { Width = 16, Height = 16, IsActive = false, VerticalAlignment = VerticalAlignment.Bottom, Margin = new Thickness(0, 0, 0, 8) };
    private readonly TextBlock _findResult = new() { TextWrapping = TextWrapping.Wrap, Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"] };
    private readonly Slider _sensitivity = new() { Header = "Mouse sensitivity", Minimum = 0.1, Maximum = 4, StepFrequency = 0.05, TickFrequency = 0.5 };
    private readonly CheckBox _altClick = new() { Content = "Alt + click sends a middle click" };
    private readonly ToggleSwitch _rawMouse = new() { Header = "Raw mouse input", OnContent = "On", OffContent = "Off" };
    private int _searchId;
    private CancellationTokenSource? _debounce;

    public HostProfile Profile { get; }

    public HostEditorDialog(KvmsCore core, HostProfile profile)
    {
        _core = core;
        Profile = profile;
        Title = profile.MisterHost.Length == 0 && profile.NdiSource.Length == 0 ? "Add host" : "Edit host";
        PrimaryButtonText = "Save";
        CloseButtonText = "Cancel";
        DefaultButton = ContentDialogButton.Primary;

        _name.Text = profile.Name;
        _misterHost.Text = profile.MisterHost;
        _misterPort.Value = profile.MisterPort;
        _obsHost.Text = profile.ObsHost;
        _ndiSource.Text = profile.NdiSource;
        _sensitivity.Value = profile.MouseSensitivity;
        _altClick.IsChecked = profile.AltClickIsMiddle;
        _rawMouse.IsOn = profile.RawMouse;

        _name.TextChanged += (_, _) => IsPrimaryButtonEnabled = _name.Text.Trim().Length > 0;
        _find.Click += (_, _) => FindSources();
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
        Opened += (_, _) => FindSources();
        PrimaryButtonClick += (_, _) => Save();

        var ndiRow = new Grid { ColumnSpacing = 8 };
        ndiRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        ndiRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        ndiRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        ndiRow.Children.Add(_ndiSource);
        Grid.SetColumn(_searching, 1);
        ndiRow.Children.Add(_searching);
        Grid.SetColumn(_find, 2);
        ndiRow.Children.Add(_find);

        var ndiNote = new TextBlock { TextWrapping = TextWrapping.Wrap, Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"] };
        ndiNote.Inlines.Add(new Run { Text = "Enter the OBS PC to list its sources directly; otherwise sources found on the local network are listed. Video uses NDI® (" });
        var link = new Hyperlink { NavigateUri = new Uri("https://ndi.video") };
        link.Inlines.Add(new Run { Text = "ndi.video" });
        ndiNote.Inlines.Add(link);
        ndiNote.Inlines.Add(new Run { Text = "). The first time, Windows Firewall may ask to allow MiSTerKVMS on private networks: allow it, or video can't arrive." });

        var rawNote = new TextBlock
        {
            Text = "Sends your mouse's own movement without Windows pointer acceleration, so only the vintage OS accelerates it, like a mouse plugged straight in.",
            TextWrapping = TextWrapping.Wrap,
            Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"],
        };

        Content = new ScrollViewer
        {
            Content = new StackPanel
            {
                Spacing = 12,
                MinWidth = 440,
                Children =
                {
                    _name,
                    Section("MiSTer (keyboard & mouse via mrext Remote)"),
                    Row(_misterHost, _misterPort, 120),
                    Section("Video & audio (OBS via NDI)"),
                    _obsHost,
                    ndiRow,
                    _findResult,
                    ndiNote,
                    Section("Mouse"),
                    _sensitivity,
                    _altClick,
                    _rawMouse,
                    rawNote,
                },
            },
        };
    }

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
        Profile.MisterHost = _misterHost.Text.Trim();
        Profile.MisterPort = double.IsNaN(_misterPort.Value) ? (ushort)8182 : (ushort)_misterPort.Value;
        Profile.ObsHost = _obsHost.Text.Trim();
        Profile.NdiSource = (_ndiSource.SelectedItem as string ?? _ndiSource.Text).Trim();
        Profile.MouseSensitivity = Math.Round(_sensitivity.Value, 2);
        Profile.AltClickIsMiddle = _altClick.IsChecked == true;
        Profile.RawMouse = _rawMouse.IsOn;
    }
}
