//! Keyboard and mouse capture. Keys come through GTK (the window forwards
//! them while capturing), with the desktop's own shortcuts inhibited so
//! Super, Alt+Tab and friends reach MiSTer. The pointer is locked and its
//! movement read in the best way the session offers:
//!
//! - Wayland: pointer-constraints + relative-pointer (raw deltas available).
//! - X11: warp to centre.
//! - Otherwise: plain motion events (the pointer can leave the window).

mod wayland;
mod x11;

use std::sync::Arc;

use gtk::gdk;
use gtk::prelude::*;
use kvms_core::MisterSession;

use wayland::WaylandPointer;
use x11::X11Pointer;

enum Pointer {
    Wayland(WaylandPointer),
    X11(X11Pointer),
    Plain { last: Option<(f64, f64)> },
}

struct Active {
    pointer: Pointer,
    toplevel: Option<gdk::Toplevel>,
    session: Arc<MisterSession>,
    video: gtk::Widget,
}

#[derive(Default)]
pub struct Capture {
    active: Option<Active>,
}

impl Capture {
    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    pub fn session(&self) -> Option<&Arc<MisterSession>> {
        self.active.as_ref().map(|a| &a.session)
    }

    /// Start capturing over `video`. Returns a note for the user when
    /// something works less well than it should (e.g. no pointer lock).
    pub fn start(&mut self, video: &gtk::Widget, session: Arc<MisterSession>, raw: bool) -> Option<String> {
        self.stop();
        let mut notes = Vec::new();
        let toplevel = video
            .native()
            .and_then(|n| n.surface())
            .and_then(|s| s.downcast::<gdk::Toplevel>().ok());
        if let Some(t) = &toplevel {
            t.inhibit_system_shortcuts(None::<&gdk::Event>);
        }
        let display = video.display();
        let pointer = if display.is::<gdk_wayland::WaylandDisplay>() {
            match WaylandPointer::start(video, session.clone(), raw) {
                Ok(p) => Pointer::Wayland(p),
                Err(e) => {
                    kvms_core::log(&format!("capture: {e}"));
                    notes.push(format!("No pointer lock ({e}): the pointer can leave the window."));
                    Pointer::Plain { last: None }
                }
            }
        } else if display.is::<gdk_x11::X11Display>() {
            if raw {
                notes.push("Raw mouse input needs Wayland: using the desktop's pointer speed.".to_string());
            }
            match X11Pointer::start(video, session.clone()) {
                Ok(p) => Pointer::X11(p),
                Err(e) => {
                    kvms_core::log(&format!("capture: {e}"));
                    Pointer::Plain { last: None }
                }
            }
        } else {
            Pointer::Plain { last: None }
        };
        video.set_cursor_from_name(Some("none"));
        video.grab_focus();
        let kind = match pointer {
            Pointer::Wayland(_) => "wayland lock",
            Pointer::X11(_) => "x11 warp",
            Pointer::Plain { .. } => "plain",
        };
        kvms_core::log(&format!("capture: start ({kind}, raw mouse {})", if raw { "on" } else { "off" }));
        self.active = Some(Active { pointer, toplevel, session, video: video.clone() });
        (!notes.is_empty()).then(|| notes.join(" "))
    }

    /// Whether the desktop agreed to pass its shortcuts on (it may ask the
    /// user first, so check a moment after starting).
    pub fn shortcuts_inhibited(&self) -> Option<bool> {
        self.active.as_ref()?.toplevel.as_ref().map(|t| t.is_shortcuts_inhibited())
    }

    pub fn stop(&mut self) {
        let Some(active) = self.active.take() else { return };
        if let Some(t) = &active.toplevel {
            t.restore_system_shortcuts();
        }
        if let Pointer::Wayland(p) = active.pointer {
            p.stop();
        }
        active.video.set_cursor(None);
        active.session.release_all();
        kvms_core::log("capture: end");
    }

    /// Pointer motion over the video (widget coordinates). Only used when the
    /// pointer isn't locked by the compositor.
    pub fn motion(&mut self, x: f64, y: f64) {
        let Some(active) = self.active.as_mut() else { return };
        match &mut active.pointer {
            Pointer::Wayland(_) => {}
            Pointer::X11(p) => p.motion(x, y),
            Pointer::Plain { last } => {
                if let Some((lx, ly)) = last.replace((x, y)) {
                    active.session.mouse_move(x - lx, y - ly);
                }
            }
        }
    }
}
