using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using System.Text.Json;
using System.Text.Json.Nodes;
using System.Text.Json.Serialization;

namespace MiSTerKVMS.Core;

/// <summary>
/// The Rust core (kvms_core.dll) through its C interface: one controller,
/// requests and replies as JSON (see core/src/capi.rs).
/// </summary>
public sealed partial class KvmsCore : IDisposable
{
    public static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web)
    {
        Converters = { new JsonStringEnumConverter(JsonNamingPolicy.CamelCase) },
    };

    private static KvmsCore? _instance;
    private IntPtr _controller;

    /// <summary>Raised on a background thread with the event name
    /// ("changed", "profilesChanged", "captureRequested", "releaseRequested").</summary>
    public event Action<string>? Event;

    public unsafe KvmsCore()
    {
        if (_instance != null) throw new InvalidOperationException("one controller per process");
        _instance = this;
        _controller = kvms_controller_new(&OnEvent, IntPtr.Zero);
    }

    [UnmanagedCallersOnly(CallConvs = [typeof(CallConvCdecl)])]
    private static void OnEvent(IntPtr user, IntPtr name)
    {
        try
        {
            _instance?.Event?.Invoke(Marshal.PtrToStringUTF8(name) ?? "");
        }
        catch (Exception e)
        {
            Log.Write($"event handler failed: {e}");
        }
    }

    /// <summary>Run a command; throws <see cref="CoreException"/> on error.</summary>
    public JsonNode? Call(string cmd, object? args = null)
    {
        var request = args == null ? new JsonObject() : JsonSerializer.SerializeToNode(args, Json)!.AsObject();
        request["cmd"] = cmd;
        IntPtr reply = kvms_call(_controller, request.ToJsonString());
        try
        {
            var node = JsonNode.Parse(Marshal.PtrToStringUTF8(reply) ?? "{}")!.AsObject();
            if (node.TryGetPropertyValue("error", out var error) && error != null)
                throw new CoreException(error.GetValue<string>());
            return node["ok"];
        }
        finally
        {
            kvms_string_free(reply);
        }
    }

    public T Call<T>(string cmd, object? args = null) =>
        Call(cmd, args).Deserialize<T>(Json)!;

    public void Dispose()
    {
        if (_controller != IntPtr.Zero)
        {
            kvms_controller_free(_controller);
            _controller = IntPtr.Zero;
        }
        _instance = null;
    }

    private const string Lib = "kvms_core";

    [LibraryImport(Lib)]
    private static unsafe partial IntPtr kvms_controller_new(delegate* unmanaged[Cdecl]<IntPtr, IntPtr, void> onEvent, IntPtr user);

    [LibraryImport(Lib)]
    private static partial void kvms_controller_free(IntPtr controller);

    [LibraryImport(Lib, StringMarshalling = StringMarshalling.Utf8)]
    private static partial IntPtr kvms_call(IntPtr controller, string request);

    [LibraryImport(Lib)]
    private static partial void kvms_string_free(IntPtr s);
}

public sealed class CoreException(string message) : Exception(message);

public enum ScaleMode { Fit, Integer, Native }

public sealed class HostProfile
{
    public string Id { get; set; } = "";
    public string Name { get; set; } = "";
    /// <summary>"capture", "local", or "" ("local" if LocalVideo is set,
    /// else "capture").</summary>
    public string Source { get; set; } = "";
    /// <summary>Capture server stream, mkvm://host/stream.</summary>
    public string Stream { get; set; } = "";
    public string LocalVideo { get; set; } = "";
    public string LocalAudio { get; set; } = "";
    /// <summary>"auto", "best", "balanced" or "low".</summary>
    public string Quality { get; set; } = "auto";
    public string MisterHost { get; set; } = "";
    public ushort MisterPort { get; set; } = 8182;
    public double MouseSensitivity { get; set; } = 1;
    public bool AltClickIsMiddle { get; set; } = true;
    public bool RawMouse { get; set; }
    public string ScaleMode { get; set; } = "fit";
    public bool SmoothScaling { get; set; }
    public bool AutoCrop { get; set; } = true;
    public bool PredictiveCursor { get; set; } = true;
    public double CursorLagMs { get; set; }
    public double CursorGain { get; set; }

    public HostProfile Clone() => (HostProfile)MemberwiseClone();

    /// <summary>Where video comes from, as the core decides it.</summary>
    [JsonIgnore]
    public string SourceKind => Source switch
    {
        "capture" or "local" => Source,
        // Older profiles may still say "ndi"; the core opens them as capture hosts.
        "ndi" => "capture",
        _ when LocalVideo.Length > 0 => "local",
        _ => "capture",
    };

    /// <summary>The stream's name, from its address.</summary>
    [JsonIgnore]
    public string StreamName
    {
        get
        {
            var path = Stream.Contains("://") ? Stream[(Stream.IndexOf("://") + 3)..] : Stream;
            int slash = path.IndexOf('/');
            return slash < 0 ? "" : Uri.UnescapeDataString(path[(slash + 1)..].TrimEnd('/'));
        }
    }

    /// <summary>The host list's second line.</summary>
    [JsonIgnore]
    public string Subtitle => SourceKind switch
    {
        "capture" => StreamName.Length > 0 ? $"Stream {StreamName}" : "No stream chosen",
        "local" => "Capture card on this PC",
        _ => MisterHost,
    };
}

/// <summary>A stream a capture server announces.</summary>
public sealed class FoundStream
{
    public string Name { get; set; } = "";
    public string Address { get; set; } = "";
    public string Mister { get; set; } = "";
    public string Server { get; set; } = "";

    public override string ToString() =>
        Server.Length > 0 ? $"{Name} — on {Server}" : Name;
}

public sealed class CaptureCard
{
    public string Id { get; set; } = "";
    public string Name { get; set; } = "";
    public string Audio { get; set; } = "";

    public override string ToString() => Name;
}

public sealed class AudioInput
{
    public string Id { get; set; } = "";
    public string Name { get; set; } = "";

    public override string ToString() => Name;
}

/// <summary>A capture server stream's connection and who has control.</summary>
public sealed class StreamState
{
    public bool Connected { get; set; }
    public string Error { get; set; } = "";
    public bool MisterConnected { get; set; }
    public string Mister { get; set; } = "";
    public bool HaveControl { get; set; }
    /// <summary>Who has control when someone else does ("" = nobody).</summary>
    public string Holder { get; set; } = "";
    public double RoundTripMs { get; set; }
}

public sealed class DisplaySettings
{
    public ScaleMode ScaleMode { get; set; }
    public bool Smooth { get; set; }
    public double Zoom { get; set; } = 1;
    public double PanX { get; set; }
    public double PanY { get; set; }
    public bool AutoCrop { get; set; } = true;
    public bool PredictiveCursor { get; set; } = true;

    public DisplaySettings Clone() => (DisplaySettings)MemberwiseClone();
}

public sealed class ConnectionState
{
    /// <summary>"connecting", "connected" or "disconnected".</summary>
    public string State { get; set; } = "disconnected";
    public string? Reason { get; set; }
}

public sealed class InputStats
{
    public bool Connected { get; set; }
    public ulong MessagesSent { get; set; }
    public double MessagesPerSec { get; set; }
    public ulong Queued { get; set; }
}

public sealed class VideoStats
{
    public string State { get; set; } = "";
    public uint SourceWidth { get; set; }
    public uint SourceHeight { get; set; }
    public uint CropWidth { get; set; }
    public uint CropHeight { get; set; }
    public double ReceivedFps { get; set; }
    public double RenderedFps { get; set; }
    public long DroppedFrames { get; set; }
    public ulong SkippedFrames { get; set; }
    public double FrameAgeMs { get; set; }
    public double AudioBufferMs { get; set; }
    public uint AudioUnderruns { get; set; }
    public uint AudioRate { get; set; }
    public bool CursorLocked { get; set; }
    public double CursorLagMs { get; set; }
    public double CursorGain { get; set; }
    public double CursorErrorPx { get; set; }
    public double CursorDetectionsPerSec { get; set; }
    public uint CursorFitSamples { get; set; }
}

public sealed class ControllerState
{
    public string? ProfileId { get; set; }
    public ConnectionState Connection { get; set; } = new();
    public string CoreRunning { get; set; } = "";
    public string GameRunning { get; set; } = "";
    public bool Capturing { get; set; }
    public bool StatsVisible { get; set; }
    public bool StatsPinned { get; set; }
    public InputStats? InputStats { get; set; }
    public VideoStats? VideoStats { get; set; }
    public string? VideoError { get; set; }
    public DisplaySettings Display { get; set; } = new();
    public bool Muted { get; set; }
    public double? RecordingSeconds { get; set; }
    public string? LastRecording { get; set; }
    public string? RecordingError { get; set; }
    /// <summary>Capture server hosts only.</summary>
    public StreamState? Stream { get; set; }

    /// <summary>Someone else controls this stream's MiSTer.</summary>
    public bool SomeoneElseInControl => Stream is { HaveControl: false } st && st.Holder.Length > 0;

    public bool IsConnected => Connection.State == "connected";
}
