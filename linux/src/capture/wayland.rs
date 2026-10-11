//! Pointer capture on Wayland: lock the pointer to our surface
//! (pointer-constraints) and read relative motion (relative-pointer), which
//! carries both accelerated and raw (unaccelerated) deltas. GTK has no API
//! for either, so this talks to the compositor on GDK's own connection.

use std::sync::Arc;
use std::time::Duration;

use gdk_wayland::prelude::*;
use gtk::glib;
use gtk::prelude::*;
use kvms_core::MisterSession;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::wp::pointer_constraints::zv1::client::zwp_locked_pointer_v1::{self, ZwpLockedPointerV1};
use wayland_protocols::wp::pointer_constraints::zv1::client::zwp_pointer_constraints_v1::{
    Lifetime, ZwpPointerConstraintsV1,
};
use wayland_protocols::wp::relative_pointer::zv1::client::zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1;
use wayland_protocols::wp::relative_pointer::zv1::client::zwp_relative_pointer_v1::{self, ZwpRelativePointerV1};

/// Events are read by GDK; ours are dispatched this often while captured.
const DISPATCH: Duration = Duration::from_millis(2);

struct State {
    session: Arc<MisterSession>,
    raw: bool,
    locked: bool,
}

pub struct WaylandPointer {
    locked_pointer: ZwpLockedPointerV1,
    relative: ZwpRelativePointerV1,
    conn: Connection,
    timer: Option<glib::SourceId>,
}

impl WaylandPointer {
    /// Lock the pointer over `widget` and send its movement to `session`.
    pub fn start(widget: &gtk::Widget, session: Arc<MisterSession>, raw: bool) -> Result<Self, String> {
        let display = widget
            .display()
            .downcast::<gdk_wayland::WaylandDisplay>()
            .map_err(|_| "not a Wayland display")?;
        let wl_display = display.wl_display().ok_or("no wl_display")?;
        let backend = wl_display.backend().upgrade().ok_or("Wayland connection gone")?;
        let conn = Connection::from_backend(backend);

        let surface = widget
            .native()
            .and_then(|n| n.surface())
            .and_then(|s| s.downcast::<gdk_wayland::WaylandSurface>().ok())
            .and_then(|s| s.wl_surface())
            .ok_or("no Wayland surface")?;
        let pointer = display
            .default_seat()
            .and_then(|s| s.downcast::<gdk_wayland::WaylandSeat>().ok())
            .and_then(|s| s.wl_pointer())
            .ok_or("no Wayland pointer")?;

        let (globals, mut queue): (_, EventQueue<State>) =
            registry_queue_init(&conn).map_err(|e| format!("registry: {e}"))?;
        let qh = queue.handle();
        let constraints: ZwpPointerConstraintsV1 = globals
            .bind(&qh, 1..=1, ())
            .map_err(|_| "the compositor doesn't support pointer locking")?;
        let manager: ZwpRelativePointerManagerV1 = globals
            .bind(&qh, 1..=1, ())
            .map_err(|_| "the compositor doesn't support relative pointer motion")?;

        let locked_pointer = constraints.lock_pointer(&surface, &pointer, None, Lifetime::Oneshot, &qh, ());
        let relative = manager.get_relative_pointer(&pointer, &qh, ());
        // The globals were only needed to make these.
        constraints.destroy();
        manager.destroy();
        conn.flush().map_err(|e| format!("flush: {e}"))?;

        let mut state = State { session, raw, locked: false };
        let timer = glib::timeout_add_local(DISPATCH, move || {
            if queue.dispatch_pending(&mut state).is_err() {
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        Ok(Self { locked_pointer, relative, conn, timer: Some(timer) })
    }

    pub fn stop(mut self) {
        self.release();
    }

    fn release(&mut self) {
        if let Some(timer) = self.timer.take() {
            timer.remove();
            self.locked_pointer.destroy();
            self.relative.destroy();
            let _ = self.conn.flush();
        }
    }
}

impl Drop for WaylandPointer {
    fn drop(&mut self) {
        self.release();
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwpPointerConstraintsV1, ()> for State {
    fn event(_: &mut Self, _: &ZwpPointerConstraintsV1, _: <ZwpPointerConstraintsV1 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwpRelativePointerManagerV1, ()> for State {
    fn event(_: &mut Self, _: &ZwpRelativePointerManagerV1, _: <ZwpRelativePointerManagerV1 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwpLockedPointerV1, ()> for State {
    fn event(state: &mut Self, _: &ZwpLockedPointerV1, event: zwp_locked_pointer_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            zwp_locked_pointer_v1::Event::Locked => {
                state.locked = true;
                kvms_core::log("capture: pointer locked");
            }
            zwp_locked_pointer_v1::Event::Unlocked => {
                state.locked = false;
                kvms_core::log("capture: pointer unlocked");
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpRelativePointerV1, ()> for State {
    fn event(state: &mut Self, _: &ZwpRelativePointerV1, event: zwp_relative_pointer_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let zwp_relative_pointer_v1::Event::RelativeMotion { dx, dy, dx_unaccel, dy_unaccel, .. } = event {
            // Accelerated deltas are in surface coordinates (points, like
            // macOS); raw ones are the mouse's own counts.
            let (x, y) = if state.raw { (dx_unaccel, dy_unaccel) } else { (dx, dy) };
            if x != 0.0 || y != 0.0 {
                state.session.mouse_move(x, y);
            }
        }
    }
}
