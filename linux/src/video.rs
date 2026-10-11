//! The video area. GTK can't give wgpu a surface, so the core renders each
//! picture into memory at this widget's exact size in pixels (crop, scaling,
//! zoom and the predictive cursor already applied) and this widget just
//! draws it 1:1.

use std::cell::{Cell, RefCell};

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, graphene, gsk};

type ResizeFn = Box<dyn Fn(u32, u32, f32)>;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct VideoView {
        pub texture: RefCell<Option<gdk::Texture>>,
        pub on_resize: RefCell<Option<ResizeFn>>,
        /// Last size reported: pixels wide, high, and pixels per point.
        pub size: Cell<(u32, u32, f32)>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for VideoView {
        const NAME: &'static str = "KvmsVideoView";
        type Type = super::VideoView;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for VideoView {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_hexpand(true);
            obj.set_vexpand(true);
            obj.set_focusable(true);
            obj.connect_scale_factor_notify(|v| v.imp().report_size());
            obj.connect_map(|v| v.imp().report_size());
        }
    }

    impl WidgetImpl for VideoView {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let rect = graphene::Rect::new(0.0, 0.0, obj.width() as f32, obj.height() as f32);
            snapshot.append_color(&gdk::RGBA::BLACK, &rect);
            if let Some(texture) = self.texture.borrow().as_ref() {
                // Already rendered at our pixel size: nearest keeps it exact.
                snapshot.append_scaled_texture(texture, gsk::ScalingFilter::Nearest, &rect);
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            self.report_size();
        }
    }

    impl VideoView {
        pub fn report_size(&self) {
            let obj = self.obj();
            // Fractional scaling (GTK 4.12+) comes from the surface.
            let scale = obj
                .native()
                .and_then(|n| n.surface())
                .map_or(obj.scale_factor() as f64, |s| s.scale());
            let w = (obj.width() as f64 * scale).round().max(1.0) as u32;
            let h = (obj.height() as f64 * scale).round().max(1.0) as u32;
            let size = (w, h, scale as f32);
            if size != self.size.get() {
                self.size.set(size);
                if let Some(f) = self.on_resize.borrow().as_ref() {
                    f(w, h, scale as f32);
                }
            }
        }
    }
}

glib::wrapper! {
    pub struct VideoView(ObjectSubclass<imp::VideoView>) @extends gtk::Widget;
}

impl Default for VideoView {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl VideoView {
    /// Called with the size in pixels and pixels per point whenever it changes.
    pub fn connect_resized(&self, f: impl Fn(u32, u32, f32) + 'static) {
        *self.imp().on_resize.borrow_mut() = Some(Box::new(f));
        self.imp().report_size();
    }

    pub fn pixel_size(&self) -> (u32, u32, f32) {
        self.imp().size.get()
    }

    /// Show a picture from the core (BGRA).
    pub fn show_frame(&self, frame: kvms_core::video::RenderedFrame) {
        let bytes = glib::Bytes::from_owned(frame.data);
        let texture = gdk::MemoryTexture::new(
            frame.width as i32,
            frame.height as i32,
            gdk::MemoryFormat::B8g8r8a8,
            &bytes,
            frame.stride as usize,
        );
        *self.imp().texture.borrow_mut() = Some(texture.upcast());
        self.queue_draw();
    }

    /// Back to black (no session).
    pub fn clear(&self) {
        self.imp().texture.borrow_mut().take();
        self.queue_draw();
    }
}
