//! Pointer capture on X11: keep the (hidden) pointer at the centre of the
//! video by warping it back after each move; the distance it travelled is
//! the movement. Keyboard grabbing comes from GDK's shortcut inhibition.

use std::sync::Arc;

use gtk::graphene;
use gtk::prelude::*;
use kvms_core::MisterSession;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt;
use x11rb::rust_connection::RustConnection;

pub struct X11Pointer {
    conn: RustConnection,
    window: u32,
    /// Centre of the video in widget coordinates (points) and in window
    /// pixels (where the pointer is warped to).
    center: (f64, f64),
    center_px: (i16, i16),
    session: Arc<MisterSession>,
}

impl X11Pointer {
    pub fn start(widget: &gtk::Widget, session: Arc<MisterSession>) -> Result<Self, String> {
        let native = widget.native().ok_or("widget not in a window")?;
        let surface = native
            .surface()
            .and_then(|s| s.downcast::<gdk_x11::X11Surface>().ok())
            .ok_or("not an X11 window")?;
        let (conn, _) = x11rb::connect(None).map_err(|e| format!("X11: {e}"))?;
        let center = (widget.width() as f64 / 2.0, widget.height() as f64 / 2.0);
        let in_native = widget
            .compute_point(&native, &graphene::Point::new(center.0 as f32, center.1 as f32))
            .ok_or("can't place the pointer")?;
        let (sx, sy) = native.surface_transform();
        let scale = widget.scale_factor() as f64;
        let center_px = (((in_native.x() as f64 + sx) * scale) as i16, ((in_native.y() as f64 + sy) * scale) as i16);
        let pointer = Self { conn, window: surface.xid() as u32, center, center_px, session };
        pointer.warp();
        Ok(pointer)
    }

    /// Pointer motion over the video, in widget coordinates.
    pub fn motion(&mut self, x: f64, y: f64) {
        let (dx, dy) = (x - self.center.0, y - self.center.1);
        // The warp back shows up as a motion to the centre: no movement.
        if dx.abs() < 0.5 && dy.abs() < 0.5 {
            return;
        }
        self.session.mouse_move(dx, dy);
        self.warp();
    }

    fn warp(&self) {
        let _ = self.conn.warp_pointer(x11rb::NONE, self.window, 0, 0, 0, 0, self.center_px.0, self.center_px.1);
        let _ = self.conn.flush();
    }
}
