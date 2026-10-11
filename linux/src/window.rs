//! The main window: hosts on the left; the video, toolbar and status bar on
//! the right. It renders the core's state and forwards input while
//! capturing.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use kvms_core::controller::{Controller, ControllerEvent, ControllerObserver, ControllerState};
use kvms_core::keymap::linux_to_hid;
use kvms_core::profiles::HostProfile;
use kvms_core::video::{FrameOutput, NativeSurface, ScaleMode};
use kvms_core::{ConnectionState, KeyResult, MouseButton};

use crate::capture::Capture;
use crate::video::VideoView;

pub const APP_ID: &str = "io.github.misterkvms.MiSTerKVMS";

const CSS: &str = "
.video-area { background-color: black; }
.video-area statuspage { color: white; }
.stats { font-family: monospace; font-size: 9pt; color: #4ce06a; }
.stats-box { background-color: rgba(0, 0, 0, 0.6); border-radius: 6px; padding: 8px; }
.hint { background-color: rgba(0, 0, 0, 0.75); color: white; border-radius: 10px; padding: 10px 16px; }
.hint .warning { color: #ffd54f; }
.status-dot.connected { color: #33cc55; }
.status-dot.connecting { color: #e6b800; }
.status-dot.disconnected { color: #e01b24; }
.recording { color: #e01b24; }
";

thread_local! {
    static UI: RefCell<Option<Rc<Ui>>> = const { RefCell::new(None) };
}

/// The window's state, reachable from core callbacks (which arrive on
/// other threads and hop to the main loop first).
fn ui() -> Option<Rc<Ui>> {
    UI.with(|u| u.borrow().clone())
}

struct Observer;

impl ControllerObserver for Observer {
    fn on_event(&self, event: ControllerEvent) {
        glib::MainContext::default().invoke(move || {
            if let Some(ui) = ui() {
                ui.on_event(event);
            }
        });
    }
}

pub struct Window;

impl Window {
    #[allow(clippy::new_ret_no_self)]
    pub fn new(app: &adw::Application) -> adw::ApplicationWindow {
        let provider = gtk::CssProvider::new();
        provider.load_from_string(CSS);
        gtk::style_context_add_provider_for_display(
            &gdk::Display::default().expect("display"),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let ui = Ui::build(app);
        UI.with(|u| *u.borrow_mut() = Some(ui.clone()));
        ui.window.clone()
    }
}

pub struct Ui {
    pub window: adw::ApplicationWindow,
    pub core: Arc<Controller>,
    frames: Arc<FrameOutput>,
    frame_pending: Arc<AtomicBool>,
    toasts: adw::ToastOverlay,
    host_list: gtk::ListBox,
    sidebar_stack: gtk::Stack,
    profiles: RefCell<Vec<HostProfile>>,
    state: RefCell<ControllerState>,
    /// Set while the host list is refilled, so that doesn't open hosts.
    updating_list: Cell<bool>,
    title: adw::WindowTitle,
    menu_button: gtk::Button,
    capture_button: gtk::Button,
    zoom_label: gtk::Button,
    record_button: gtk::Button,
    mute_button: gtk::ToggleButton,
    video: VideoView,
    placeholder: adw::StatusPage,
    capture_hint: gtk::Box,
    hint_note: gtk::Label,
    stats_box: gtk::Box,
    stats_label: gtk::Label,
    stats_pin: gtk::Button,
    status_dot: gtk::Label,
    status_label: gtk::Label,
    recording_label: gtk::Label,
    last_recording: gtk::Button,
    game_label: gtk::Label,
    capture_label: gtk::Label,
    capture: RefCell<Capture>,
    hint_timer: RefCell<Option<glib::SourceId>>,
}

impl Ui {
    fn build(app: &adw::Application) -> Rc<Self> {
        let frame_pending = Arc::new(AtomicBool::new(false));
        let pending = frame_pending.clone();
        let frames = FrameOutput::new(move || {
            // One wake-up per batch of frames; the newest is drawn.
            if !pending.swap(true, Ordering::AcqRel) {
                glib::MainContext::default().invoke(|| {
                    if let Some(ui) = ui() {
                        ui.on_frame();
                    }
                });
            }
        });
        let core = Controller::new(Arc::new(Observer));
        core.set_frame_output(Some(frames.clone()));
        kvms_core::log(&format!("NDI: {}", kvms_core::video::ndi_runtime_info()));
        kvms_core::log(&format!("settings: {}", core.settings_path()));

        // Sidebar: hosts.
        let host_list = gtk::ListBox::new();
        host_list.add_css_class("navigation-sidebar");
        let add_button = gtk::Button::from_icon_name("list-add-symbolic");
        add_button.set_tooltip_text(Some("Add a host"));
        add_button.set_action_name(Some("win.add-host"));
        let sidebar_header = adw::HeaderBar::new();
        sidebar_header.set_title_widget(Some(&adw::WindowTitle::new("Hosts", "")));
        sidebar_header.pack_end(&add_button);
        let add_first = gtk::Button::with_label("Add Host…");
        add_first.set_action_name(Some("win.add-host"));
        add_first.set_halign(gtk::Align::Center);
        add_first.add_css_class("pill");
        let empty = adw::StatusPage::builder()
            .icon_name("video-display-symbolic")
            .title("No Hosts")
            .description("Add a MiSTer and the NDI source that carries its video.")
            .child(&add_first)
            .build();
        empty.add_css_class("compact");
        let sidebar_stack = gtk::Stack::new();
        sidebar_stack.add_named(&gtk::ScrolledWindow::builder().child(&host_list).vexpand(true).build(), Some("list"));
        sidebar_stack.add_named(&empty, Some("empty"));
        let sidebar = adw::ToolbarView::new();
        sidebar.add_top_bar(&sidebar_header);
        sidebar.set_content(Some(&sidebar_stack));

        // Header bar.
        let title = adw::WindowTitle::new("MiSTerKVMS", "");
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&title));
        let menu_button = gtk::Button::with_label("F12 Menu");
        menu_button.set_action_name(Some("win.mister-menu"));
        menu_button.set_tooltip_text(Some("Sends F12, which opens the MiSTer menu (OSD). You can also press F12 while capturing."));
        let capture_button = gtk::Button::from_icon_name("input-mouse-symbolic");
        capture_button.set_action_name(Some("win.capture"));
        header.pack_start(&menu_button);
        header.pack_start(&capture_button);

        let main_menu = gio::Menu::new();
        let session_section = gio::Menu::new();
        session_section.append(Some("Send Ctrl+Alt+Del"), Some("win.ctrl-alt-del"));
        session_section.append(Some("Edit Host…"), Some("win.edit-host"));
        session_section.append(Some("Show Last Recording"), Some("win.show-recording"));
        main_menu.append_section(None, &session_section);
        let stats_section = gio::Menu::new();
        stats_section.append(Some("Show Stats"), Some("win.show-stats"));
        stats_section.append(Some("Pin Stats Overlay"), Some("win.pin-stats"));
        main_menu.append_section(None, &stats_section);
        let app_section = gio::Menu::new();
        app_section.append(Some("Export Hosts…"), Some("win.export"));
        app_section.append(Some("Import Hosts…"), Some("win.import"));
        app_section.append(Some("About MiSTerKVMS"), Some("win.about"));
        main_menu.append_section(None, &app_section);
        let main_menu_button = gtk::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(&main_menu).build();
        main_menu_button.set_tooltip_text(Some("Main menu"));

        let view_menu = gio::Menu::new();
        let scale_section = gio::Menu::new();
        scale_section.append(Some("Fit"), Some("win.scale::fit"));
        scale_section.append(Some("Integer (sharpest)"), Some("win.scale::integer"));
        scale_section.append(Some("1:1"), Some("win.scale::native"));
        view_menu.append_section(Some("Scale"), &scale_section);
        let options = gio::Menu::new();
        options.append(Some("Smooth Scaling"), Some("win.smooth"));
        options.append(Some("Auto-crop Borders"), Some("win.auto-crop"));
        options.append(Some("Predictive Cursor"), Some("win.predictive"));
        view_menu.append_section(None, &options);
        let view_button = gtk::MenuButton::builder()
            .icon_name("video-display-symbolic")
            .menu_model(&view_menu)
            .tooltip_text("Scale and picture options")
            .build();

        let zoom_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        zoom_box.add_css_class("linked");
        let zoom_out = gtk::Button::from_icon_name("zoom-out-symbolic");
        zoom_out.set_action_name(Some("win.zoom-out"));
        zoom_out.set_tooltip_text(Some("Zoom out (Ctrl+−)"));
        let zoom_label = gtk::Button::with_label("100%");
        zoom_label.set_action_name(Some("win.zoom-reset"));
        zoom_label.set_tooltip_text(Some("Reset zoom (Ctrl+0)"));
        let zoom_in = gtk::Button::from_icon_name("zoom-in-symbolic");
        zoom_in.set_action_name(Some("win.zoom-in"));
        zoom_in.set_tooltip_text(Some("Zoom in (Ctrl+=)"));
        zoom_box.append(&zoom_out);
        zoom_box.append(&zoom_label);
        zoom_box.append(&zoom_in);

        let record_button = gtk::Button::from_icon_name("media-record-symbolic");
        record_button.set_action_name(Some("win.record"));
        let mute_button = gtk::ToggleButton::new();
        mute_button.set_icon_name("audio-volume-high-symbolic");
        mute_button.set_action_name(Some("win.mute"));
        mute_button.set_tooltip_text(Some("Mute (Ctrl+Shift+M)"));
        header.pack_end(&main_menu_button);
        header.pack_end(&mute_button);
        header.pack_end(&record_button);
        header.pack_end(&zoom_box);
        header.pack_end(&view_button);

        // Video and overlays.
        let video = VideoView::default();
        let placeholder = adw::StatusPage::builder().icon_name("video-display-symbolic").build();
        placeholder.set_can_target(false);
        let capture_hint = gtk::Box::new(gtk::Orientation::Vertical, 4);
        capture_hint.add_css_class("hint");
        capture_hint.set_halign(gtk::Align::Center);
        capture_hint.set_valign(gtk::Align::Start);
        capture_hint.set_margin_top(12);
        capture_hint.set_can_target(false);
        capture_hint.set_visible(false);
        let hint_title = gtk::Label::new(Some("Keyboard and mouse go to MiSTer"));
        hint_title.add_css_class("heading");
        let hint_keys = gtk::Label::new(None);
        hint_keys.set_markup("Press <b>Right Super + Right Alt + Esc</b> to release · <b>F12</b> opens the MiSTer menu");
        let hint_note = gtk::Label::new(None);
        hint_note.add_css_class("warning");
        hint_note.set_wrap(true);
        hint_note.set_visible(false);
        capture_hint.append(&hint_title);
        capture_hint.append(&hint_keys);
        capture_hint.append(&hint_note);

        let stats_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        stats_box.add_css_class("stats-box");
        stats_box.set_halign(gtk::Align::Start);
        stats_box.set_valign(gtk::Align::End);
        stats_box.set_margin_start(12);
        stats_box.set_margin_bottom(12);
        stats_box.set_visible(false);
        let stats_label = gtk::Label::new(None);
        stats_label.add_css_class("stats");
        stats_label.set_xalign(0.0);
        stats_label.set_valign(gtk::Align::Start);
        let stats_pin = gtk::Button::from_icon_name("view-pin-symbolic");
        stats_pin.add_css_class("flat");
        stats_pin.set_valign(gtk::Align::Start);
        stats_pin.set_action_name(Some("win.pin-stats"));
        stats_box.append(&stats_label);
        stats_box.append(&stats_pin);

        let overlay = gtk::Overlay::new();
        overlay.add_css_class("video-area");
        overlay.set_child(Some(&video));
        overlay.add_overlay(&placeholder);
        overlay.add_overlay(&capture_hint);
        overlay.add_overlay(&stats_box);

        // Status bar.
        let status = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        status.set_margin_start(12);
        status.set_margin_end(12);
        status.set_margin_top(6);
        status.set_margin_bottom(6);
        let small = |text: &str| {
            let l = gtk::Label::new(Some(text));
            l.add_css_class("caption");
            l
        };
        let status_dot = small("●");
        status_dot.add_css_class("status-dot");
        let status_label = small("Not connected");
        let recording_label = small("");
        recording_label.add_css_class("recording");
        let last_recording = gtk::Button::new();
        last_recording.add_css_class("flat");
        last_recording.add_css_class("caption");
        last_recording.set_action_name(Some("win.show-recording"));
        last_recording.set_visible(false);
        let game_label = small("");
        game_label.add_css_class("dim-label");
        game_label.set_hexpand(true);
        game_label.set_xalign(1.0);
        let capture_label = small("");
        capture_label.add_css_class("dim-label");
        status.append(&status_dot);
        status.append(&status_label);
        status.append(&recording_label);
        status.append(&last_recording);
        status.append(&game_label);
        status.append(&capture_label);

        let content = adw::ToolbarView::new();
        content.add_top_bar(&header);
        content.set_content(Some(&overlay));
        content.add_bottom_bar(&status);

        let split = adw::OverlaySplitView::new();
        split.set_sidebar(Some(&sidebar));
        split.set_content(Some(&content));
        split.set_min_sidebar_width(220.0);
        split.set_max_sidebar_width(280.0);
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&split));

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("MiSTerKVMS")
            .default_width(1280)
            .default_height(800)
            .content(&toasts)
            .icon_name(APP_ID)
            .build();

        let ui = Rc::new(Self {
            window,
            core,
            frames,
            frame_pending,
            toasts,
            host_list,
            sidebar_stack,
            profiles: RefCell::new(Vec::new()),
            state: RefCell::new(ControllerState {
                profile_id: None,
                connection: ConnectionState::Disconnected { reason: "not connected".into() },
                core_running: String::new(),
                game_running: String::new(),
                capturing: false,
                stats_visible: false,
                stats_pinned: false,
                input_stats: None,
                video_stats: None,
                video_error: None,
                display: Default::default(),
                muted: false,
                recording_seconds: None,
                last_recording: None,
                recording_error: None,
            }),
            updating_list: Cell::new(false),
            title,
            menu_button,
            capture_button,
            zoom_label,
            record_button,
            mute_button,
            video,
            placeholder,
            capture_hint,
            hint_note,
            stats_box,
            stats_label,
            stats_pin,
            status_dot,
            status_label,
            recording_label,
            last_recording,
            game_label,
            capture_label,
            capture: RefCell::new(Capture::default()),
            hint_timer: RefCell::new(None),
        });
        ui.connect();
        ui.load_profiles();
        ui.render();
        ui
    }

    /// Signals, actions and input controllers.
    fn connect(self: &Rc<Self>) {
        let core = self.core.clone();
        self.video.connect_resized(move |w, h, scale| {
            core.attach_surface(NativeSurface::Offscreen, w, h, scale);
        });

        self.host_list.connect_row_selected(|_, row| {
            let Some(ui) = ui() else { return };
            if ui.updating_list.get() {
                return;
            }
            let id = row.map(|r| r.widget_name().to_string());
            match id {
                Some(id) => {
                    ui.stop_capture();
                    ui.core.open(id);
                }
                None => ui.core.close(),
            }
        });

        // Click the video to capture.
        let click = gtk::GestureClick::new();
        click.connect_pressed(|_, _, _, _| {
            if let Some(ui) = ui() {
                ui.start_capture();
            }
        });
        self.video.add_controller(click);

        // Pointer motion when the compositor doesn't lock the pointer for us.
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(|_, x, y| {
            if let Some(ui) = ui() {
                ui.capture.borrow_mut().motion(x, y);
            }
        });
        self.video.add_controller(motion);

        // Not capturing: Ctrl+wheel zooms, the wheel pans when zoomed in.
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        scroll.connect_scroll(|controller, dx, dy| {
            let Some(ui) = ui() else { return glib::Propagation::Proceed };
            if ui.capture.borrow().is_active() {
                return glib::Propagation::Stop;
            }
            let ctrl = controller.current_event_state().contains(gdk::ModifierType::CONTROL_MASK);
            let zoom = ui.state.borrow().display.zoom;
            if ctrl {
                ui.core.zoom_by(1.25f64.powf(-dy));
            } else if zoom > 1.0 {
                ui.core.pan_by(-0.05 * dx / zoom, -0.05 * dy / zoom);
            }
            glib::Propagation::Stop
        });
        self.video.add_controller(scroll);

        // While capturing, every key goes to MiSTer (before GTK shortcuts).
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(|_, _, keycode, _| key(keycode, true));
        keys.connect_key_released(|_, _, keycode, _| {
            key(keycode, false);
        });
        self.window.add_controller(keys);

        // ...and every mouse button.
        let buttons = gtk::EventControllerLegacy::new();
        buttons.set_propagation_phase(gtk::PropagationPhase::Capture);
        buttons.connect_event(|_, event| {
            let Some(ui) = ui() else { return glib::Propagation::Proceed };
            let capture = ui.capture.borrow();
            let Some(session) = capture.session() else { return glib::Propagation::Proceed };
            match event.event_type() {
                gdk::EventType::ButtonPress | gdk::EventType::ButtonRelease => {
                    let down = event.event_type() == gdk::EventType::ButtonPress;
                    let button = event.downcast_ref::<gdk::ButtonEvent>().map_or(0, |b| b.button());
                    let button = match button {
                        1 => Some(MouseButton::Left),
                        2 => Some(MouseButton::Middle),
                        3 => Some(MouseButton::Right),
                        _ => None,
                    };
                    if let Some(b) = button {
                        session.mouse_button(b, down);
                    }
                    glib::Propagation::Stop
                }
                // mrext has no wheel command yet.
                gdk::EventType::Scroll => glib::Propagation::Stop,
                _ => glib::Propagation::Proceed,
            }
        });
        self.window.add_controller(buttons);

        // Release everything when another window takes over.
        self.window.connect_is_active_notify(|w| {
            if !w.is_active() {
                if let Some(ui) = ui() {
                    ui.stop_capture();
                }
            }
        });
        self.window.connect_close_request(|_| {
            if let Some(ui) = ui() {
                ui.stop_capture();
                ui.core.close();
                ui.core.detach_surface();
            }
            UI.with(|u| u.borrow_mut().take());
            glib::Propagation::Proceed
        });

        self.add_actions();
    }

    fn add_actions(self: &Rc<Self>) {
        let w = &self.window;
        let simple = |name: &str, f: fn(&Rc<Ui>)| {
            let action = gio::SimpleAction::new(name, None);
            action.connect_activate(move |_, _| {
                if let Some(ui) = ui() {
                    f(&ui);
                }
            });
            w.add_action(&action);
        };
        simple("add-host", |ui| ui.edit_host(None));
        simple("edit-host", |ui| {
            let id = ui.state.borrow().profile_id.clone();
            ui.edit_host(id);
        });
        simple("mister-menu", |ui| ui.core.open_menu());
        simple("ctrl-alt-del", |ui| ui.core.send_ctrl_alt_del());
        simple("capture", |ui| {
            if ui.capture.borrow().is_active() {
                ui.stop_capture();
            } else {
                ui.start_capture();
            }
        });
        simple("zoom-in", |ui| ui.core.zoom_by(1.25));
        simple("zoom-out", |ui| ui.core.zoom_by(1.0 / 1.25));
        simple("zoom-reset", |ui| ui.core.reset_zoom());
        simple("record", |ui| ui.toggle_recording());
        simple("mute", |ui| {
            let muted = ui.state.borrow().muted;
            ui.core.set_muted(!muted);
        });
        simple("show-stats", |ui| ui.core.flash_stats());
        simple("pin-stats", |ui| {
            let pinned = ui.state.borrow().stats_pinned;
            ui.core.set_stats_pinned(!pinned);
        });
        simple("show-recording", |ui| {
            if let Some(path) = ui.state.borrow().last_recording.clone() {
                let file = gio::File::for_path(&path);
                gtk::FileLauncher::new(Some(&file)).open_containing_folder(Some(&ui.window), gio::Cancellable::NONE, |_| {});
            }
        });
        simple("export", |ui| ui.export_profiles());
        simple("import", |ui| ui.import_profiles());
        simple("about", |ui| crate::about::show(&ui.window));

        let scale = gio::SimpleAction::new_stateful("scale", Some(glib::VariantTy::STRING), &"fit".to_variant());
        scale.connect_activate(|_, param| {
            let (Some(ui), Some(mode)) = (ui(), param.and_then(|p| p.str().map(str::to_string))) else { return };
            let mut d = ui.state.borrow().display.clone();
            d.scale_mode = ScaleMode::from_name(&mode);
            ui.core.set_display(d);
        });
        w.add_action(&scale);
        let toggle = |name: &str, f: fn(&mut kvms_core::DisplaySettings)| {
            let action = gio::SimpleAction::new_stateful(name, None, &false.to_variant());
            action.connect_activate(move |_, _| {
                if let Some(ui) = ui() {
                    let mut d = ui.state.borrow().display.clone();
                    f(&mut d);
                    ui.core.set_display(d);
                }
            });
            w.add_action(&action);
        };
        toggle("smooth", |d| d.smooth = !d.smooth);
        toggle("auto-crop", |d| d.auto_crop = !d.auto_crop);
        toggle("predictive", |d| d.predictive_cursor = !d.predictive_cursor);

        let app = self.window.application().expect("application");
        for (action, accels) in [
            ("win.capture", &["<Control><Shift>k"][..]),
            ("win.show-stats", &["<Control>i"]),
            ("win.pin-stats", &["<Control><Shift>i"]),
            ("win.record", &["<Control><Shift>r"]),
            ("win.mute", &["<Control><Shift>m"]),
            ("win.zoom-in", &["<Control>equal", "<Control>plus", "<Control>KP_Add"]),
            ("win.zoom-out", &["<Control>minus", "<Control>KP_Subtract"]),
            ("win.zoom-reset", &["<Control>0"]),
        ] {
            app.set_accels_for_action(action, accels);
        }
    }

    // MARK: Core events and rendering

    fn on_event(self: &Rc<Self>, event: ControllerEvent) {
        match event {
            ControllerEvent::Changed => self.render(),
            ControllerEvent::ProfilesChanged => {
                self.load_profiles();
                self.render();
            }
            ControllerEvent::CaptureRequested => {
                // Auto-capture on connect, if the user is looking at us.
                if self.window.is_active() {
                    self.start_capture();
                }
            }
            ControllerEvent::ReleaseRequested => self.stop_capture(),
        }
    }

    fn on_frame(&self) {
        self.frame_pending.store(false, Ordering::Release);
        if let Some(frame) = self.frames.take() {
            if self.state.borrow().profile_id.is_some() {
                self.video.show_frame(frame);
            }
        }
    }

    fn load_profiles(&self) {
        let profiles = self.core.profiles();
        self.updating_list.set(true);
        while let Some(row) = self.host_list.row_at_index(0) {
            self.host_list.remove(&row);
        }
        let open = self.state.borrow().profile_id.clone();
        for p in &profiles {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&p.name))
                .subtitle(if p.mister_host.is_empty() { "No address".to_string() } else { p.mister_host.clone() })
                .build();
            row.set_widget_name(&p.id);
            let menu = gio::Menu::new();
            menu.append(Some("Edit…"), Some(&format!("win.edit-host-id::{}", p.id)));
            menu.append(Some("Delete"), Some(&format!("win.delete-host::{}", p.id)));
            let more = gtk::MenuButton::builder()
                .icon_name("view-more-symbolic")
                .menu_model(&menu)
                .valign(gtk::Align::Center)
                .build();
            more.add_css_class("flat");
            row.add_suffix(&more);
            self.host_list.append(&row);
            if open.as_deref() == Some(p.id.as_str()) {
                self.host_list.select_row(Some(&row));
            }
        }
        self.updating_list.set(false);
        self.sidebar_stack.set_visible_child_name(if profiles.is_empty() { "empty" } else { "list" });
        *self.profiles.borrow_mut() = profiles;
        self.add_row_actions();
    }

    /// Per-host menu actions (they take the host id).
    fn add_row_actions(&self) {
        if self.window.lookup_action("edit-host-id").is_some() {
            return;
        }
        let edit = gio::SimpleAction::new("edit-host-id", Some(glib::VariantTy::STRING));
        edit.connect_activate(|_, p| {
            if let (Some(ui), Some(id)) = (ui(), p.and_then(|p| p.str().map(str::to_string))) {
                ui.edit_host(Some(id));
            }
        });
        self.window.add_action(&edit);
        let delete = gio::SimpleAction::new("delete-host", Some(glib::VariantTy::STRING));
        delete.connect_activate(|_, p| {
            if let (Some(ui), Some(id)) = (ui(), p.and_then(|p| p.str().map(str::to_string))) {
                ui.confirm_delete(id);
            }
        });
        self.window.add_action(&delete);
    }

    fn open_profile(&self) -> Option<HostProfile> {
        let id = self.state.borrow().profile_id.clone()?;
        self.profiles.borrow().iter().find(|p| p.id == id).cloned()
    }

    fn render(&self) {
        let s = self.core.state();
        let was_open = self.state.borrow().profile_id.clone();
        if was_open != s.profile_id {
            self.video.clear();
        }
        *self.state.borrow_mut() = s.clone();
        let profile = self.open_profile();
        let open = profile.is_some();
        let connected = s.connection == ConnectionState::Connected;
        let video_up = s.video_stats.as_ref().is_some_and(|v| v.state == "connected");

        self.title.set_title(profile.as_ref().map_or("MiSTerKVMS", |p| p.name.as_str()));
        self.title.set_subtitle(&match &profile {
            None => String::new(),
            Some(p) if s.core_running.is_empty() => p.mister_host.clone(),
            Some(_) => s.core_running.clone(),
        });

        let set_enabled = |name: &str, on: bool| {
            if let Some(a) = self.window.lookup_action(name).and_then(|a| a.downcast::<gio::SimpleAction>().ok()) {
                a.set_enabled(on);
            }
        };
        for name in ["edit-host", "scale", "smooth", "auto-crop", "predictive", "zoom-in", "zoom-out", "zoom-reset", "mute", "show-stats"] {
            set_enabled(name, open);
        }
        set_enabled("mister-menu", connected);
        set_enabled("ctrl-alt-del", connected);
        set_enabled("capture", connected || s.capturing);
        set_enabled("record", s.recording_seconds.is_some() || video_up);
        set_enabled("show-recording", s.last_recording.is_some());
        let set_state = |name: &str, v: glib::Variant| {
            if let Some(a) = self.window.lookup_action(name).and_then(|a| a.downcast::<gio::SimpleAction>().ok()) {
                a.set_state(&v);
            }
        };
        set_state("scale", s.display.scale_mode.name().to_variant());
        set_state("smooth", s.display.smooth.to_variant());
        set_state("auto-crop", s.display.auto_crop.to_variant());
        set_state("predictive", s.display.predictive_cursor.to_variant());

        self.capture_button.set_icon_name(if s.capturing { "process-stop-symbolic" } else { "input-mouse-symbolic" });
        self.capture_button.set_tooltip_text(Some(if s.capturing {
            "Release keyboard and mouse (Right Super + Right Alt + Esc)"
        } else {
            "Send keyboard and mouse to MiSTer (or click the screen) — Ctrl+Shift+K"
        }));
        self.menu_button.set_sensitive(connected);
        self.zoom_label.set_label(&format!("{}%", (s.display.zoom * 100.0).round()));
        let recording = s.recording_seconds.is_some();
        self.record_button.set_icon_name(if recording { "media-playback-stop-symbolic" } else { "media-record-symbolic" });
        self.record_button.set_tooltip_text(Some(if recording {
            "Stop recording (Ctrl+Shift+R)"
        } else {
            "Record video and audio to Videos/MiSTerKVMS (Ctrl+Shift+R)"
        }));
        if self.mute_button.is_active() != s.muted {
            self.mute_button.set_active(s.muted);
        }
        self.mute_button.set_icon_name(if s.muted { "audio-volume-muted-symbolic" } else { "audio-volume-high-symbolic" });
        self.stats_pin.set_icon_name(if s.stats_pinned { "view-pin-symbolic" } else { "pin-symbolic" });

        self.render_placeholder(profile.as_ref(), &s, video_up);
        self.render_stats(&s, open);
        self.render_status(&s);
    }

    fn render_placeholder(&self, profile: Option<&HostProfile>, s: &ControllerState, video_up: bool) {
        self.placeholder.set_visible(!video_up);
        let connected = s.connection == ConnectionState::Connected;
        let hint = if connected && !s.capturing { "\n\nClick here to capture keyboard and mouse" } else { "" };
        let (title, detail) = match profile {
            None if self.profiles.borrow().is_empty() => ("Add a host to get started".to_string(), String::new()),
            None => ("Select a Host".to_string(), "Choose a MiSTer in the sidebar to connect.".to_string()),
            Some(p) if p.ndi_source.is_empty() => (
                "No NDI source set".to_string(),
                "Edit the host and choose the OBS NDI source for this MiSTer.".to_string(),
            ),
            Some(_) if s.video_error.is_some() => ("Video unavailable".to_string(), s.video_error.clone().unwrap_or_default()),
            Some(p) => (
                p.ndi_source.clone(),
                match s.video_stats.as_ref().map(|v| v.state.as_str()) {
                    None | Some("searching") => "Looking for the NDI source…".to_string(),
                    Some("no video") => "Connected, waiting for video".to_string(),
                    Some(other) => other.to_string(),
                },
            ),
        };
        self.placeholder.set_title(&title);
        self.placeholder.set_description(Some(&glib::markup_escape_text(&format!("{detail}{hint}"))));
    }

    fn render_stats(&self, s: &ControllerState, open: bool) {
        self.stats_box.set_visible(open && s.stats_visible);
        if !self.stats_box.is_visible() {
            return;
        }
        let mut lines = vec!["INPUT".to_string()];
        if let Some(i) = &s.input_stats {
            lines.push(format!("link   {}", if i.connected { "up" } else { "down" }));
            lines.push(format!("msg/s  {:.0}", i.messages_per_sec));
            lines.push(format!("sent   {}", i.messages_sent));
            lines.push(format!("queue  {}", i.queued));
        }
        lines.push(String::new());
        lines.push("VIDEO".to_string());
        match &s.video_stats {
            Some(v) => {
                lines.push(format!("state  {}", v.state));
                lines.push(format!("src    {}×{}", v.source_width, v.source_height));
                lines.push(format!("crop   {}×{}", v.crop_width, v.crop_height));
                lines.push(format!("fps    {:.1} in / {:.1} drawn", v.received_fps, v.rendered_fps));
                lines.push(format!("drop   {} ndi / {} skipped", v.dropped_frames, v.skipped_frames));
                lines.push(format!("age    {}", age_text(v.frame_age_ms)));
                lines.push(format!("audio  {:.0} ms buf · {} underruns", v.audio_buffer_ms, v.audio_underruns));
                if s.display.predictive_cursor {
                    lines.push(format!(
                        "cursor {} · err {:.0} px",
                        if v.cursor_locked { "tracked" } else { "searching" },
                        v.cursor_error_px
                    ));
                    lines.push(format!(
                        "model  lag {:.0} ms · ×{:.2} · {} fits/min",
                        v.cursor_lag_ms, v.cursor_gain, v.cursor_fit_samples
                    ));
                }
            }
            None => lines.push("off".to_string()),
        }
        self.stats_label.set_text(&lines.join("\n"));
    }

    fn render_status(&self, s: &ControllerState) {
        for c in ["connected", "connecting", "disconnected"] {
            self.status_dot.remove_css_class(c);
        }
        let text = match &s.connection {
            ConnectionState::Connected => {
                self.status_dot.add_css_class("connected");
                if s.core_running.is_empty() {
                    "Connected · MiSTer menu".to_string()
                } else {
                    format!("Connected · {}", s.core_running)
                }
            }
            ConnectionState::Connecting => {
                self.status_dot.add_css_class("connecting");
                "Connecting…".to_string()
            }
            ConnectionState::Disconnected { reason } => {
                self.status_dot.add_css_class("disconnected");
                format!("Disconnected: {reason}")
            }
        };
        self.status_label.set_text(&text);
        match (s.recording_seconds, &s.recording_error, &s.last_recording) {
            (Some(secs), _, _) => {
                let secs = secs as u64;
                self.recording_label.set_text(&format!("● {}:{:02}", secs / 60, secs % 60));
                self.last_recording.set_visible(false);
            }
            (None, Some(e), _) => {
                self.recording_label.set_text(&format!("Recording failed: {e}"));
                self.last_recording.set_visible(false);
            }
            (None, None, Some(path)) => {
                self.recording_label.set_text("");
                let name = std::path::Path::new(path).file_name().map_or(path.clone(), |n| n.to_string_lossy().into());
                self.last_recording.set_label(&format!("Saved {name} · Show in Folder"));
                self.last_recording.set_visible(true);
            }
            _ => {
                self.recording_label.set_text("");
                self.last_recording.set_visible(false);
            }
        }
        self.game_label.set_text(&s.game_running);
        self.capture_label.set_text(if s.capturing {
            "Capturing — Right Super + Right Alt + Esc to release"
        } else {
            "F12 = MiSTer menu · not capturing"
        });
    }

    // MARK: Capture

    fn start_capture(self: &Rc<Self>) {
        let raw = self.open_profile().is_some_and(|p| p.raw_mouse);
        let Some(session) = self.core.input_session() else { return };
        if !self.core.is_connected() || self.capture.borrow().is_active() {
            return;
        }
        let note = self.capture.borrow_mut().start(self.video.upcast_ref(), session, raw);
        self.core.set_capturing(true);
        self.show_hint(note);
    }

    fn stop_capture(&self) {
        if self.capture.borrow().is_active() {
            self.capture.borrow_mut().stop();
            self.core.set_capturing(false);
        }
        self.capture_hint.set_visible(false);
    }

    fn show_hint(self: &Rc<Self>, note: Option<String>) {
        self.hint_note.set_text(note.as_deref().unwrap_or(""));
        self.hint_note.set_visible(note.is_some());
        self.capture_hint.set_visible(true);
        if let Some(t) = self.hint_timer.borrow_mut().take() {
            t.remove();
        }
        // The desktop may ask before passing its shortcuts on; say so if it didn't.
        let timer = glib::timeout_add_local_once(Duration::from_secs(4), || {
            if let Some(ui) = ui() {
                ui.hint_timer.borrow_mut().take();
                ui.capture_hint.set_visible(false);
                if ui.capture.borrow().shortcuts_inhibited() == Some(false) {
                    ui.toast("The desktop kept its own shortcuts (Super, Alt+Tab): they won't reach MiSTer.");
                }
            }
        });
        *self.hint_timer.borrow_mut() = Some(timer);
    }

    // MARK: Hosts

    fn edit_host(self: &Rc<Self>, id: Option<String>) {
        self.stop_capture();
        let profile = match &id {
            Some(id) => match self.profiles.borrow().iter().find(|p| &p.id == id) {
                Some(p) => p.clone(),
                None => return,
            },
            None => kvms_core::profiles::new_host_profile(),
        };
        crate::editor::show(&self.window, profile, id.is_none(), |saved, is_new| {
            if let Some(ui) = ui() {
                let id = saved.id.clone();
                ui.core.save_profile(saved);
                if is_new {
                    ui.core.open(id);
                    ui.load_profiles();
                }
            }
        });
    }

    fn confirm_delete(&self, id: String) {
        let Some(name) = self.profiles.borrow().iter().find(|p| p.id == id).map(|p| p.name.clone()) else { return };
        let dialog = adw::AlertDialog::new(Some(&format!("Delete “{name}”?")), Some("The host's settings are removed."));
        dialog.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_close_response("cancel");
        dialog.connect_response(None, move |_, response| {
            if response == "delete" {
                if let Some(ui) = ui() {
                    ui.core.delete_profile(id.clone());
                }
            }
        });
        dialog.present(Some(&self.window));
    }

    fn export_profiles(&self) {
        let dialog = gtk::FileDialog::builder().title("Export Hosts").initial_name("MiSTerKVMS hosts.json").build();
        dialog.save(Some(&self.window), gio::Cancellable::NONE, |result| {
            let (Ok(file), Some(ui)) = (result, ui()) else { return };
            let Some(path) = file.path() else { return };
            match std::fs::write(&path, ui.core.export_profiles()) {
                Ok(()) => ui.toast("Hosts exported."),
                Err(e) => ui.toast(&format!("Export failed: {e}")),
            }
        });
    }

    fn import_profiles(&self) {
        let dialog = gtk::FileDialog::builder().title("Import Hosts").build();
        dialog.open(Some(&self.window), gio::Cancellable::NONE, |result| {
            let (Ok(file), Some(ui)) = (result, ui()) else { return };
            let Some(path) = file.path() else { return };
            let message = match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|json| {
                ui.core.import_profiles(json).map_err(|e| e.to_string())
            }) {
                Ok(1) => "Imported 1 host.".to_string(),
                Ok(n) => format!("Imported {n} hosts."),
                Err(e) => format!("That file couldn't be read: {e}"),
            };
            ui.toast(&message);
        });
    }

    // MARK: Recording

    fn toggle_recording(&self) {
        if self.state.borrow().recording_seconds.is_some() {
            self.core.stop_recording();
            return;
        }
        let Some(profile) = self.open_profile() else { return };
        let dir = glib::user_special_dir(glib::UserDirectory::Videos)
            .unwrap_or_else(glib::home_dir)
            .join("MiSTerKVMS");
        let stamp = glib::DateTime::now_local()
            .and_then(|t| t.format("%Y-%m-%d %H.%M.%S"))
            .map(|s| s.to_string())
            .unwrap_or_default();
        let path = kvms_core::video::ffmpeg::recording_path(&dir, &profile.name, &stamp);
        if let Err(e) = self.core.start_recording(path.display().to_string()) {
            self.toast(&format!("Can't record: {e}"));
        }
    }

    pub fn toast(&self, message: &str) {
        self.toasts.add_toast(adw::Toast::new(message));
    }
}

/// A key from the window's key controller. While capturing it goes to
/// MiSTer and GTK never sees it.
fn key(keycode: u32, down: bool) -> glib::Propagation {
    let Some(ui) = ui() else { return glib::Propagation::Proceed };
    let session = match ui.capture.borrow().session() {
        Some(s) => s.clone(),
        None => return glib::Propagation::Proceed,
    };
    // GDK key codes are evdev codes + 8 on both Wayland and X11.
    if let Some(usage) = keycode.checked_sub(8).and_then(|c| linux_to_hid(c as u16)) {
        if session.key_event(usage, down) == KeyResult::ExitCapture {
            ui.stop_capture();
        }
    }
    glib::Propagation::Stop
}

/// NDI frame age needs the OBS PC's and this computer's clocks to agree;
/// over a second almost always means they don't.
fn age_text(ms: f64) -> String {
    if ms < 0.0 {
        "n/a".into()
    } else if ms > 1000.0 {
        format!("clocks differ (~{:.1} s)", ms / 1000.0)
    } else {
        format!("≈{ms:.0} ms OBS→screen")
    }
}
