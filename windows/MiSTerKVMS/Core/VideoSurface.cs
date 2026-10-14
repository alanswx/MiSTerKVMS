using System.Runtime.InteropServices;
using Microsoft.UI.Composition;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Hosting;
using WinRT;

namespace MiSTerKVMS.Core;

/// <summary>
/// Shows the core's video inside a XAML element. The core renders into a
/// DirectComposition surface handle (a swap chain it owns); this wraps the
/// handle in a composition brush on a visual behind the element's children,
/// so XAML (overlays, dialogs) draws on top of the video, which a child
/// window couldn't do.
/// </summary>
public sealed class VideoSurface : IDisposable
{
    private readonly KvmsCore _core;
    private readonly FrameworkElement _host;
    private readonly SpriteVisual _visual;
    private readonly ICompositionSurface _surface;
    private readonly CompositionSurfaceBrush _brush;
    private readonly ulong _handle;
    private (uint W, uint H, float Scale) _size;

    public VideoSurface(KvmsCore core, FrameworkElement host)
    {
        _core = core;
        _host = host;
        _handle = core.Call<ulong>("createCompositionSurface");
        var compositor = ElementCompositionPreview.GetElementVisual(host).Compositor;
        _surface = SurfaceForHandle(compositor, _handle);
        _brush = compositor.CreateSurfaceBrush(_surface);
        // The swap chain is sized in pixels to match the element exactly.
        _brush.Stretch = CompositionStretch.Fill;
        _visual = compositor.CreateSpriteVisual();
        _visual.Brush = _brush;
        _visual.RelativeSizeAdjustment = System.Numerics.Vector2.One;
        ElementCompositionPreview.SetElementChildVisual(host, _visual);

        host.SizeChanged += (_, _) => UpdateSize();
        host.Loaded += (_, _) => UpdateSize();
        UpdateSize();
    }

    /// <summary>Call when the DPI may have changed (XamlRoot.Changed).</summary>
    public void UpdateSize()
    {
        float scale = (float)(_host.XamlRoot?.RasterizationScale ?? 1.0);
        uint w = (uint)Math.Max(1, Math.Round(_host.ActualWidth * scale));
        uint h = (uint)Math.Max(1, Math.Round(_host.ActualHeight * scale));
        if ((w, h, scale) == _size) return;
        bool first = _size.W == 0;
        _size = (w, h, scale);
        if (w < 2 || h < 2) return;
        if (first)
            _core.Call("attachSurface", new
            {
                surface = new { kind = "compositionHandle", handle = _handle },
                width = w,
                height = h,
                scale,
            });
        else
            _core.Call("resizeSurface", new { width = w, height = h, scale });
    }

    public void Dispose()
    {
        ElementCompositionPreview.SetElementChildVisual(_host, null);
        // Stop the renderer before the handle goes away.
        _core.Call("detachSurface");
        _brush.Dispose();
        _visual.Dispose();
        (_surface as IDisposable)?.Dispose();
        _core.Call("closeCompositionSurface", new { handle = _handle });
    }

    // ICompositorSwapChainInterop (Microsoft.UI.Composition.Interop.h):
    // IUnknown (3) + CreateGraphicsDevice, then CreateCompositionSurfaceForHandle.
    private static readonly Guid IID_ICompositorSwapChainInterop = new("FC084699-67D8-40E1-ADE7-08901D84FFDA");

    private static unsafe ICompositionSurface SurfaceForHandle(Compositor compositor, ulong handle)
    {
        IntPtr unknown = ((IWinRTObject)compositor).NativeObject.ThisPtr;
        Guid iid = IID_ICompositorSwapChainInterop;
        Marshal.ThrowExceptionForHR(Marshal.QueryInterface(unknown, in iid, out IntPtr interop));
        try
        {
            var vtable = *(IntPtr**)interop;
            var create = (delegate* unmanaged[Stdcall]<IntPtr, IntPtr, IntPtr*, int>)vtable[4];
            IntPtr result;
            Marshal.ThrowExceptionForHR(create(interop, (IntPtr)handle, &result));
            try
            {
                return MarshalInterface<ICompositionSurface>.FromAbi(result);
            }
            finally
            {
                Marshal.Release(result);
            }
        }
        finally
        {
            Marshal.Release(interop);
        }
    }
}
