using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Documents;
using Microsoft.UI.Xaml.Media.Imaging;

namespace MiSTerKVMS;

/// <summary>Version and credits (the same as the macOS app's).</summary>
public sealed class AboutDialog : ContentDialog
{
    public AboutDialog()
    {
        CloseButtonText = "Close";
        var panel = new StackPanel { Spacing = 12, MaxWidth = 420 };

        var icon = Path.Combine(AppContext.BaseDirectory, "Assets", "AppIcon.png");
        if (File.Exists(icon))
            panel.Children.Add(new Image { Source = new BitmapImage(new Uri(icon)), Width = 128, Height = 128 });
        panel.Children.Add(new TextBlock
        {
            Text = "MiSTerKVMS",
            Style = (Style)Application.Current.Resources["TitleTextBlockStyle"],
            HorizontalAlignment = HorizontalAlignment.Center,
        });
        panel.Children.Add(Secondary($"Version {App.Version}", center: true));
        panel.Children.Add(new TextBlock
        {
            Text = "Keyboard, mouse, video and sound for your MiSTer FPGAs, over the network.",
            TextWrapping = TextWrapping.Wrap,
            TextAlignment = TextAlignment.Center,
        });

        Credit(panel, "MiSTer-Kun mascot",
            "Original design by HeWhoisRed, a gift to the MiSTer community. Remastered artwork by ",
            ("baxysquare/mister_kun", "https://github.com/baxysquare/mister_kun"), ".");
        Credit(panel, "MiSTer FPGA",
            ("MiSTer FPGA project", "https://github.com/MiSTer-devel/Main_MiSTer/wiki"), " by Sorgelig and contributors.");
        Credit(panel, "Keyboard & mouse",
            ("mrext Remote", "https://github.com/wizzomafizzo/mrext"), " by wizzo and contributors.");
        Credit(panel, "Built with", "Rust: wgpu, cpal, tungstenite. C#: WinUI 3 and the Windows App SDK.");

        var footer = Secondary("MIT License · ", center: true);
        AddLink(footer, "Source on GitHub", "https://github.com/danifunker/MiSTerKVMS");
        panel.Children.Add(footer);

        Content = new ScrollViewer { Content = panel };
    }

    private static TextBlock Secondary(string text, bool center = false) => new()
    {
        Text = text,
        TextWrapping = TextWrapping.Wrap,
        TextAlignment = center ? TextAlignment.Center : TextAlignment.Left,
        Foreground = (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["TextFillColorSecondaryBrush"],
    };

    /// <summary>A heading and a paragraph; parts are text or (label, url) links.</summary>
    private static void Credit(StackPanel panel, string title, params object[] parts)
    {
        var block = new StackPanel { Spacing = 2 };
        block.Children.Add(new TextBlock { Text = title, Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"] });
        var text = Secondary("");
        foreach (var part in parts)
        {
            if (part is (string label, string url)) AddLink(text, label, url);
            else text.Inlines.Add(new Run { Text = (string)part });
        }
        block.Children.Add(text);
        panel.Children.Add(block);
    }

    private static void AddLink(TextBlock text, string label, string url)
    {
        var link = new Hyperlink { NavigateUri = new Uri(url) };
        link.Inlines.Add(new Run { Text = label });
        text.Inlines.Add(link);
    }
}
