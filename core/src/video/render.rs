//! Draws video frames into a native view with wgpu (Metal on macOS,
//! D3D12/Vulkan elsewhere): crop, fit/integer/native scale, zoom/pan,
//! nearest or smooth filtering.

use std::ffi::c_void;
use std::num::NonZeroIsize;
use std::ptr::NonNull;

use raw_window_handle::{
    AppKitDisplayHandle, AppKitWindowHandle, RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle,
    WaylandWindowHandle, Win32WindowHandle, WindowsDisplayHandle, XlibDisplayHandle, XlibWindowHandle,
};

use super::crop::Crop;
use super::cursor::PointerState;
use std::sync::Arc;

use super::{DisplaySettings, FrameOutput, NativeSurface, RenderedFrame, ScaleMode};

const SHADER: &str = r#"
struct U { rect: vec4<f32>, uv: vec4<f32>, tint: vec4<f32> };
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VO { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex fn vs(@builtin(vertex_index) i: u32) -> VO {
    let c = vec2<f32>(f32(i & 1u), f32(i >> 1u));
    var o: VO;
    o.pos = vec4<f32>(mix(u.rect.x, u.rect.z, c.x), mix(u.rect.y, u.rect.w, c.y), 0.0, 1.0);
    o.uv = vec2<f32>(mix(u.uv.x, u.uv.z, c.x), mix(u.uv.y, u.uv.w, c.y));
    return o;
}

@fragment fn fs(i: VO) -> @location(0) vec4<f32> {
    return vec4<f32>(textureSample(tex, samp, i.uv).rgb, 1.0);
}

// The arrow's white fill takes the tint; the black outline stays black.
@fragment fn fs_pointer(i: VO) -> @location(0) vec4<f32> {
    let c = textureSample(tex, samp, i.uv);
    return vec4<f32>(c.rgb * u.tint.rgb, c.a * u.tint.a);
}
"#;

/// Our predicted cursor, in source-frame pixels.
pub struct Pointer {
    pub at: (f64, f64),
    /// Guest cursor size if known; our arrow is drawn at that height.
    pub size: Option<(f64, f64)>,
    pub state: PointerState,
}

/// Fill colours: black when lined up with the guest's cursor, green while
/// ahead of it, amber when the guest's cursor isn't being tracked.
fn tint(state: PointerState) -> [f32; 4] {
    match state {
        PointerState::Synced => [0.1, 0.1, 0.1, 1.0],
        PointerState::Predicting => [0.35, 1.0, 0.45, 1.0],
        PointerState::Lost => [1.0, 0.72, 0.2, 1.0],
    }
}

/// Classic arrow pointer: B = black outline, W = white fill.
const ARROW: [&str; 19] = [
    "B...........",
    "BB..........",
    "BWB.........",
    "BWWB........",
    "BWWWB.......",
    "BWWWWB......",
    "BWWWWWB.....",
    "BWWWWWWB....",
    "BWWWWWWWB...",
    "BWWWWWWWWB..",
    "BWWWWWWWWWB.",
    "BWWWWWWBBBBB",
    "BWWWBWWB....",
    "BWWB.BWWB...",
    "BWB..BWWB...",
    "BB....BWWB..",
    "B.....BWWB..",
    ".......BWWB.",
    "........BB..",
];

fn arrow_rgba() -> (u32, u32, Vec<u8>) {
    let (w, h) = (ARROW[0].len() as u32, ARROW.len() as u32);
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for row in ARROW {
        for c in row.bytes() {
            px.extend_from_slice(match c {
                b'B' => &[0, 0, 0, 255],
                b'W' => &[255, 255, 255, 235],
                _ => &[0, 0, 0, 0],
            });
        }
    }
    (w, h, px)
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    rect: [f32; 4],
    uv: [f32; 4],
    tint: [f32; 4],
}

/// Where frames go: a native view's surface, or an offscreen texture that's
/// read back for shells that draw the picture themselves (GTK).
enum Target {
    Surface(wgpu::Surface<'static>),
    Offscreen {
        out: Arc<FrameOutput>,
        texture: Option<wgpu::Texture>,
        readback: Option<wgpu::Buffer>,
    },
}

pub struct Renderer {
    target: Target,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    uniforms: wgpu::Buffer,
    nearest: wgpu::Sampler,
    linear: wgpu::Sampler,
    texture: Option<(wgpu::Texture, u32, u32)>,
    bind_nearest: Option<wgpu::BindGroup>,
    bind_linear: Option<wgpu::BindGroup>,
    pointer_pipeline: wgpu::RenderPipeline,
    pointer_uniforms: wgpu::Buffer,
    pointer_bind: wgpu::BindGroup,
    pointer_size: (u32, u32),
}

/// Creates the GPU surface on the caller's thread (must be the main thread for
/// AppKit); everything else can then move to the render thread.
///
/// # Safety
/// The handles in `target` must be valid and outlive the returned surface.
/// `Offscreen` has no surface.
pub unsafe fn create_surface(
    target: &NativeSurface,
) -> Result<(wgpu::Instance, Option<wgpu::Surface<'static>>), String> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let ptr = |v: u64, what: &str| NonNull::new(v as usize as *mut c_void).ok_or(format!("null {what}"));
    let raw = |display, window| wgpu::SurfaceTargetUnsafe::RawHandle {
        raw_display_handle: Some(display),
        raw_window_handle: window,
    };
    let target = match *target {
        NativeSurface::AppKit { ns_view } => raw(
            RawDisplayHandle::AppKit(AppKitDisplayHandle::new()),
            RawWindowHandle::AppKit(AppKitWindowHandle::new(ptr(ns_view, "view")?)),
        ),
        NativeSurface::Win32 { hwnd, hinstance } => {
            let mut handle = Win32WindowHandle::new(NonZeroIsize::new(hwnd as isize).ok_or("null hwnd")?);
            handle.hinstance = NonZeroIsize::new(hinstance as isize);
            raw(RawDisplayHandle::Windows(WindowsDisplayHandle::new()), RawWindowHandle::Win32(handle))
        }
        #[cfg(windows)]
        NativeSurface::CompositionHandle { handle } => {
            wgpu::SurfaceTargetUnsafe::SurfaceHandle(ptr(handle, "surface handle")?.as_ptr())
        }
        #[cfg(not(windows))]
        NativeSurface::CompositionHandle { .. } => return Err("composition surfaces need Windows".into()),
        NativeSurface::Wayland { display, surface } => raw(
            RawDisplayHandle::Wayland(WaylandDisplayHandle::new(ptr(display, "display")?)),
            RawWindowHandle::Wayland(WaylandWindowHandle::new(ptr(surface, "surface")?)),
        ),
        NativeSurface::Offscreen => return Ok((instance, None)),
        NativeSurface::Xlib { display, window } => raw(
            RawDisplayHandle::Xlib(XlibDisplayHandle::new(Some(ptr(display, "display")?), 0)),
            RawWindowHandle::Xlib(XlibWindowHandle::new(window as std::os::raw::c_ulong)),
        ),
    };
    let surface = instance
        .create_surface_unsafe(target)
        .map_err(|e| format!("create surface: {e}"))?;
    Ok((instance, Some(surface)))
}

impl Renderer {
    pub fn new(
        instance: &wgpu::Instance,
        surface: Option<wgpu::Surface<'static>>,
        frames: Option<Arc<FrameOutput>>,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: surface.as_ref(),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .map_err(|e| format!("no GPU adapter: {e}"))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("kvms"),
            ..Default::default()
        }))
        .map_err(|e| format!("request device: {e}"))?;

        let (target, config) = match surface {
            Some(surface) => {
                let mut config = surface
                    .get_default_config(&adapter, width.max(1), height.max(1))
                    .ok_or("surface not supported by adapter")?;
                // Video values are already display-encoded: use a non-sRGB
                // format so they pass through untouched.
                let caps = surface.get_capabilities(&adapter);
                if let Some(f) = caps.formats.iter().find(|f| !f.is_srgb()) {
                    config.format = *f;
                }
                config.present_mode = wgpu::PresentMode::Fifo;
                config.desired_maximum_frame_latency = 1;
                surface.configure(&device, &config);
                (Target::Surface(surface), config)
            }
            None => {
                let out = frames.ok_or("offscreen rendering needs a frame output")?;
                let config = wgpu::SurfaceConfiguration {
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    format: wgpu::TextureFormat::Bgra8Unorm,
                    width: width.max(1),
                    height: height.max(1),
                    present_mode: wgpu::PresentMode::Fifo,
                    desired_maximum_frame_latency: 1,
                    alpha_mode: wgpu::CompositeAlphaMode::Auto,
                    view_formats: vec![],
                    color_space: Default::default(),
                };
                (Target::Offscreen { out, texture: None, readback: None }, config)
            }
        };

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("video"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("video"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    // The pointer's fragment shader reads the tint.
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("video"),
            bind_group_layouts: &[Some(&layout)],
            ..Default::default()
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("video"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(config.format.into())],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let uniform_buffer = |label| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: std::mem::size_of::<Uniforms>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let uniforms = uniform_buffer("video uniforms");

        // Predicted-cursor overlay: same shader, alpha-blended.
        let pointer_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("pointer"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_pointer"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let pointer_uniforms = uniform_buffer("pointer uniforms");
        let (aw, ah, arrow) = arrow_rgba();
        let pointer_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pointer"),
            size: wgpu::Extent3d { width: aw, height: ah, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &pointer_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &arrow,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(aw * 4), rows_per_image: Some(ah) },
            wgpu::Extent3d { width: aw, height: ah, depth_or_array_layers: 1 },
        );
        let sampler = |filter| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                mag_filter: filter,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            })
        };
        let nearest = sampler(wgpu::FilterMode::Nearest);
        let linear = sampler(wgpu::FilterMode::Linear);
        let pointer_view = pointer_texture.create_view(&Default::default());
        let pointer_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pointer"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: pointer_uniforms.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&pointer_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&linear) },
            ],
        });

        // Setup errors above panic (caught by the caller and shown); later
        // ones are logged rather than taking down the render thread.
        device.on_uncaptured_error(std::sync::Arc::new(|e| eprintln!("wgpu error: {e}")));

        Ok(Self {
            target,
            device,
            queue,
            config,
            pipeline,
            layout,
            uniforms,
            nearest,
            linear,
            texture: None,
            bind_nearest: None,
            bind_linear: None,
            pointer_pipeline,
            pointer_uniforms,
            pointer_bind,
            pointer_size: (aw, ah),
        })
    }

    /// Show black (when the session ends, so the last frame doesn't linger).
    pub fn clear(&mut self) {
        self.texture = None;
        let _ = self.draw(Crop::full(1, 1), &DisplaySettings::default(), 1.0, None);
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        let (width, height) = (width.max(1), height.max(1));
        if (width, height) != (self.config.width, self.config.height) {
            self.config.width = width;
            self.config.height = height;
            match &mut self.target {
                Target::Surface(surface) => surface.configure(&self.device, &self.config),
                // Recreated at the new size on the next draw.
                Target::Offscreen { texture, readback, .. } => {
                    *texture = None;
                    *readback = None;
                }
            }
        }
    }

    /// Upload a BGRA/BGRX frame.
    pub fn upload(&mut self, data: &[u8], stride: u32, width: u32, height: u32) {
        if !matches!(self.texture, Some((_, w, h)) if w == width && h == height) {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("video frame"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Bgra8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let bind = |sampler: &wgpu::Sampler| {
                self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("video"),
                    layout: &self.layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: self.uniforms.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                        wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(sampler) },
                    ],
                })
            };
            self.bind_nearest = Some(bind(&self.nearest));
            self.bind_linear = Some(bind(&self.linear));
            self.texture = Some((texture, width, height));
        }
        let (texture, _, _) = self.texture.as_ref().unwrap();
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
    }

    /// Draw the current frame. `points_scale` is the display's pixels per
    /// point (2 on Retina), used for the Native scale mode.
    pub fn draw(
        &mut self,
        crop: Crop,
        display: &DisplaySettings,
        points_scale: f32,
        pointer: Option<Pointer>,
    ) -> Result<(), String> {
        let (frame, target) = match &mut self.target {
            Target::Surface(surface) => {
                // Every path that doesn't present still submits: frame uploads
                // (`write_texture`) wait in the queue, each with its own
                // staging buffer, until the next submit. Skipping it while the
                // window is hidden (display asleep, minimised, another Space)
                // piled up ~8 MB per 1080p frame until the system ran out of
                // memory.
                let frame = match surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
                    wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                        surface.configure(&self.device, &self.config);
                        self.queue.submit([]);
                        return Ok(());
                    }
                    // Window hidden or GPU busy: skip this frame.
                    wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                        self.queue.submit([]);
                        return Ok(());
                    }
                    other => {
                        self.queue.submit([]);
                        return Err(format!("surface: {other:?}"));
                    }
                };
                let view = frame.texture.create_view(&Default::default());
                (Some(frame), view)
            }
            Target::Offscreen { texture, .. } => {
                let texture = texture.get_or_insert_with(|| {
                    self.device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("offscreen"),
                        size: wgpu::Extent3d { width: self.config.width, height: self.config.height, depth_or_array_layers: 1 },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: self.config.format,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                        view_formats: &[],
                    })
                });
                (None, texture.create_view(&Default::default()))
            }
        };
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("video"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            if let Some((_, tw, th)) = self.texture {
                let (vw, vh) = (self.config.width, self.config.height);
                let place = place(crop, vw, vh, display, points_scale);
                let u = Uniforms {
                    rect: place.ndc(vw, vh),
                    uv: [
                        crop.x as f32 / tw as f32,
                        crop.y as f32 / th as f32,
                        (crop.x + crop.w) as f32 / tw as f32,
                        (crop.y + crop.h) as f32 / th as f32,
                    ],
                    tint: [1.0; 4],
                };
                self.queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&u));
                let bind = if display.smooth { &self.bind_linear } else { &self.bind_nearest };
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, bind.as_ref(), &[]);
                pass.draw(0..4, 0..1);

                if let Some(p) = pointer {
                    // Hotspot (arrow tip) at the predicted position, drawn as
                    // tall as the guest's cursor so the two look alike.
                    let (aw, ah) = (self.pointer_size.0 as f32, self.pointer_size.1 as f32);
                    let src_h = p.size.map_or(ah * 1.5, |s| s.1 as f32);
                    let h = src_h * place.scale;
                    let w = h * aw / ah;
                    let x = place.x0 + (p.at.0 as f32 - crop.x as f32) * place.scale;
                    let y = place.y0 + (p.at.1 as f32 - crop.y as f32) * place.scale;
                    let r = Rect { x0: x, y0: y, x1: x + w, y1: y + h, scale: place.scale };
                    let u = Uniforms { rect: r.ndc(vw, vh), uv: [0.0, 0.0, 1.0, 1.0], tint: tint(p.state) };
                    self.queue.write_buffer(&self.pointer_uniforms, 0, bytemuck::bytes_of(&u));
                    pass.set_pipeline(&self.pointer_pipeline);
                    pass.set_bind_group(0, &self.pointer_bind, &[]);
                    pass.draw(0..4, 0..1);
                }
            }
        }
        match frame {
            Some(frame) => {
                self.queue.submit([encoder.finish()]);
                self.queue.present(frame);
                Ok(())
            }
            None => self.read_back(encoder),
        }
    }

    /// Offscreen: copy the picture into a buffer, wait for it, and hand it
    /// to the shell (BGRA, rows `stride` bytes apart).
    fn read_back(&mut self, mut encoder: wgpu::CommandEncoder) -> Result<(), String> {
        let Target::Offscreen { out, texture: Some(texture), readback } = &mut self.target else {
            return Ok(());
        };
        let (w, h) = (self.config.width, self.config.height);
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let stride = (w * 4).div_ceil(align) * align;
        let buffer = readback.get_or_insert_with(|| {
            self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("readback"),
                size: stride as u64 * h as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            })
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(stride), rows_per_image: Some(h) },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| format!("readback: {e}"))?;
        rx.recv().map_err(|e| e.to_string())?.map_err(|e| format!("readback: {e}"))?;
        let mut data = out.spare();
        data.clear();
        data.extend_from_slice(&slice.get_mapped_range().map_err(|e| format!("readback: {e:?}"))?);
        buffer.unmap();
        out.put(RenderedFrame { width: w, height: h, stride, data });
        Ok(())
    }
}

/// A rectangle in viewport pixels, plus the source→viewport scale.
struct Rect {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    scale: f32,
}

impl Rect {
    fn ndc(&self, vw: u32, vh: u32) -> [f32; 4] {
        let (vw, vh) = (vw as f32, vh as f32);
        [self.x0 / vw * 2.0 - 1.0, 1.0 - self.y0 / vh * 2.0, self.x1 / vw * 2.0 - 1.0, 1.0 - self.y1 / vh * 2.0]
    }
}

/// Where the cropped picture goes in the viewport.
fn place(crop: Crop, view_w: u32, view_h: u32, d: &DisplaySettings, points_scale: f32) -> Rect {
    let (cw, ch) = (crop.w.max(1) as f32, crop.h.max(1) as f32);
    let (vw, vh) = (view_w as f32, view_h as f32);
    let fit = (vw / cw).min(vh / ch);
    let base = match d.scale_mode {
        ScaleMode::Fit => fit,
        // Largest whole multiple that fits; shrink to fit if even 1× is too big.
        ScaleMode::Integer => {
            if fit >= 1.0 {
                fit.floor()
            } else {
                fit
            }
        }
        ScaleMode::Native => points_scale.max(1.0),
    };
    let scale = base * d.zoom.max(0.05) as f32;
    let (w, h) = (cw * scale, ch * scale);
    let cx = vw / 2.0 - d.pan_x as f32 * w;
    let cy = vh / 2.0 - d.pan_y as f32 * h;
    // Snap to whole pixels so nearest filtering stays even.
    let x0 = (cx - w / 2.0).round();
    let y0 = (cy - h / 2.0).round();
    Rect { x0, y0, x1: x0 + w.round(), y1: y0 + h.round(), scale }
}
