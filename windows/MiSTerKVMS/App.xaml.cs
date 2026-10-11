using Microsoft.UI.Xaml;
using MiSTerKVMS.Core;

namespace MiSTerKVMS;

public partial class App : Application
{
    public static string Version { get; } =
        typeof(App).Assembly.GetName().Version is { } v ? $"{v.Major}.{v.Minor}.{v.Build}" : "dev";

    private MainWindow? _window;

    public App()
    {
        Log.Start(Version);
        UnhandledException += (_, e) => Log.Write($"unhandled: {e.Exception}");
        InitializeComponent();
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        _window = new MainWindow();
        _window.Activate();
    }
}
