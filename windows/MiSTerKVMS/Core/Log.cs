using System.Runtime.InteropServices;
using System.Text;

namespace MiSTerKVMS.Core;

/// <summary>
/// The log file, %LOCALAPPDATA%\MiSTerKVMS\MiSTerKVMS.log. The core writes
/// to stderr, which a GUI app doesn't have, so stderr is pointed at the file.
/// Both sides write through one append-only handle, so lines never overwrite
/// each other.
/// </summary>
public static partial class Log
{
    private static IntPtr _handle = IntPtr.Zero;

    public static string FilePath { get; } = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "MiSTerKVMS", "MiSTerKVMS.log");

    public static void Start(string version)
    {
        try
        {
            Directory.CreateDirectory(Path.GetDirectoryName(FilePath)!);
            // Start fresh past 5 MB.
            if (File.Exists(FilePath) && new FileInfo(FilePath).Length > 5_000_000) File.Delete(FilePath);
            var handle = CreateFileW(FilePath, FILE_APPEND_DATA, FILE_SHARE_READ | FILE_SHARE_WRITE, IntPtr.Zero,
                OPEN_ALWAYS, 0, IntPtr.Zero);
            if (handle == new IntPtr(-1)) return;
            _handle = handle;
            SetStdHandle(STD_ERROR_HANDLE, handle);
            Write($"--- MiSTerKVMS {version} started {DateTime.Now:yyyy-MM-dd HH:mm:ss}");
        }
        catch
        {
            // No log is better than no app.
        }
    }

    public static void Write(string message)
    {
        if (_handle == IntPtr.Zero) return;
        var t = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds() / 1000.0;
        var bytes = Encoding.UTF8.GetBytes($"[{t:F3}] {message}\n");
        WriteFile(_handle, bytes, bytes.Length, out _, IntPtr.Zero);
    }

    private const int STD_ERROR_HANDLE = -12;
    private const uint FILE_APPEND_DATA = 0x0004;
    private const uint FILE_SHARE_READ = 1, FILE_SHARE_WRITE = 2;
    private const uint OPEN_ALWAYS = 4;

    [LibraryImport("kernel32.dll", SetLastError = true, StringMarshalling = StringMarshalling.Utf16)]
    private static partial IntPtr CreateFileW(string name, uint access, uint share, IntPtr security, uint disposition,
        uint flags, IntPtr template);

    [LibraryImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool WriteFile(IntPtr handle, byte[] buffer, int length, out int written, IntPtr overlapped);

    [LibraryImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool SetStdHandle(int stdHandle, IntPtr handle);
}
