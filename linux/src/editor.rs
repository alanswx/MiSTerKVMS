//! Add or edit a host: its capture server stream, the MiSTer, mouse options.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use kvms_core::discover::FoundStream;
use kvms_core::profiles::HostProfile;

/// What a profile saves as its quality, and how the editor shows it.
const QUALITIES: [(&str, &str); 4] = [
    ("auto", "Automatic"),
    ("best", "Best (full size, for wired networks)"),
    ("balanced", "Balanced (up to 1080p)"),
    ("low", "Low (up to 720p, for slow Wi-Fi)"),
];

pub fn show(parent: &impl IsA<gtk::Widget>, profile: HostProfile, is_new: bool, on_save: impl Fn(HostProfile, bool) + 'static) {
    let name = adw::EntryRow::builder().title("Name").text(profile.name.as_str()).build();

    // Streams found on the network, and the address actually saved (picking
    // one fills it in; it can also be typed).
    let found = gtk::StringList::new(&[]);
    let streams: Rc<RefCell<Vec<FoundStream>>> = Rc::default();
    let pick = adw::ComboRow::builder().title("Stream").model(&found).build();
    pick.set_selected(gtk::INVALID_LIST_POSITION);
    let find = gtk::Button::builder().icon_name("view-refresh-symbolic").valign(gtk::Align::Center).build();
    find.set_tooltip_text(Some("Look for streams again"));
    find.add_css_class("flat");
    let spinner = gtk::Spinner::builder().spinning(true).build();
    spinner.set_visible(false);
    pick.add_suffix(&spinner);
    pick.add_suffix(&find);
    let stream = adw::EntryRow::builder().title("Address (pc/stream or mkvm://pc/stream)").text(profile.stream.as_str()).build();
    let quality = adw::ComboRow::builder()
        .title("Quality")
        .model(&gtk::StringList::new(&QUALITIES.map(|(_, label)| label)))
        .build();
    quality.set_selected(QUALITIES.iter().position(|(value, _)| *value == profile.quality).unwrap_or(0) as u32);
    let video = adw::PreferencesGroup::builder()
        .title("Video and audio")
        .description(
            "Streams come from MiSTerKVMS Capture running on the PC with the capture card. Several people can \
             watch; one at a time controls the MiSTer.",
        )
        .build();
    video.add(&pick);
    video.add(&stream);
    video.add(&quality);

    let mister_host = adw::EntryRow::builder().title("MiSTer IP or hostname").text(profile.mister_host.as_str()).build();
    let port = adw::SpinRow::with_range(1.0, 65535.0, 1.0);
    port.set_title("mrext port");
    port.set_value(profile.mister_port as f64);
    let mister = adw::PreferencesGroup::builder()
        .title("MiSTer")
        .description(
            "Optional: keyboard and mouse go through the capture server, which knows its MiSTer. Without a stream, \
             the one showing this MiSTer is used.",
        )
        .build();
    mister.add(&mister_host);
    mister.add(&port);

    let sensitivity = adw::SpinRow::with_range(0.1, 4.0, 0.05);
    sensitivity.set_title("Mouse sensitivity");
    sensitivity.set_digits(2);
    sensitivity.set_value(profile.mouse_sensitivity);
    let alt_click = adw::SwitchRow::builder().title("Alt + click sends a middle click").active(profile.alt_click_is_middle).build();
    let raw_mouse = adw::SwitchRow::builder()
        .title("Raw mouse input")
        .subtitle("Send the mouse's own movement without the desktop's acceleration, so only the vintage OS accelerates it (Wayland).")
        .active(profile.raw_mouse)
        .build();
    let mouse = adw::PreferencesGroup::builder().title("Mouse").build();
    mouse.add(&sensitivity);
    mouse.add(&alt_click);
    mouse.add(&raw_mouse);

    let page = adw::PreferencesPage::new();
    let general = adw::PreferencesGroup::new();
    general.add(&name);
    page.add(&general);
    page.add(&video);
    page.add(&mister);
    page.add(&mouse);

    let cancel = gtk::Button::with_label("Cancel");
    let save = gtk::Button::with_label("Save");
    save.add_css_class("suggested-action");
    let header = adw::HeaderBar::new();
    header.set_show_end_title_buttons(false);
    header.set_show_start_title_buttons(false);
    header.pack_start(&cancel);
    header.pack_end(&save);
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&page));

    let dialog = adw::Dialog::builder()
        .title(if is_new { "Add Host" } else { "Edit Host" })
        .content_width(520)
        .content_height(640)
        .child(&view)
        .build();

    // Set while the code itself changes the picker or the address, so their
    // handlers don't answer each other (or a refilled list's auto-selection).
    let syncing = Rc::new(Cell::new(false));
    // The picker shows the found stream the address is, if any.
    let sync_pick: Rc<dyn Fn()> = {
        let (pick, stream, streams, syncing) = (pick.clone(), stream.clone(), streams.clone(), syncing.clone());
        Rc::new(move || {
            let text = stream.text();
            let at = streams.borrow().iter().position(|f| f.address == text.trim());
            syncing.set(true);
            pick.set_selected(at.map_or(gtk::INVALID_LIST_POSITION, |i| i as u32));
            syncing.set(false);
        })
    };
    {
        let (sync_pick, syncing) = (sync_pick.clone(), syncing.clone());
        stream.connect_changed(move |_| {
            if !syncing.get() {
                sync_pick();
            }
        });
    }
    {
        let (stream, mister_host, streams, syncing) = (stream.clone(), mister_host.clone(), streams.clone(), syncing.clone());
        pick.connect_selected_notify(move |pick| {
            if syncing.get() {
                return;
            }
            let Some(f) = streams.borrow().get(pick.selected() as usize).cloned() else { return };
            syncing.set(true);
            stream.set_text(&f.address);
            syncing.set(false);
            // The stream knows its MiSTer.
            if !f.mister.is_empty() {
                mister_host.set_text(&f.mister);
            }
        });
    }

    // Stream discovery (mDNS) blocks for a couple of seconds: run it on a
    // thread; a search that finishes after a newer one started is ignored.
    let search_id = Rc::new(Cell::new(0u32));
    let search = {
        let (stream, found, streams, pick, find, spinner, search_id, syncing, sync_pick) = (
            stream.clone(),
            found.clone(),
            streams.clone(),
            pick.clone(),
            find.clone(),
            spinner.clone(),
            search_id.clone(),
            syncing.clone(),
            sync_pick.clone(),
        );
        move || {
            let id = search_id.get() + 1;
            search_id.set(id);
            spinner.set_visible(true);
            find.set_sensitive(false);
            pick.set_subtitle("Looking for streams…");
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(kvms_core::discover::find_streams(2500));
            });
            let (stream, found, streams, pick, find, spinner, search_id, syncing, sync_pick) = (
                stream.clone(),
                found.clone(),
                streams.clone(),
                pick.clone(),
                find.clone(),
                spinner.clone(),
                search_id.clone(),
                syncing.clone(),
                sync_pick.clone(),
            );
            glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                let Ok(list) = rx.try_recv() else { return glib::ControlFlow::Continue };
                if id != search_id.get() {
                    return glib::ControlFlow::Break;
                }
                spinner.set_visible(false);
                find.set_sensitive(true);
                let labels: Vec<String> = list
                    .iter()
                    .map(|f| if f.mister.is_empty() { f.name.clone() } else { format!("{} ({})", f.name, f.mister) })
                    .collect();
                let count = list.len();
                *streams.borrow_mut() = list;
                syncing.set(true);
                found.splice(0, found.n_items(), &labels.iter().map(String::as_str).collect::<Vec<_>>());
                syncing.set(false);
                pick.set_subtitle(&match count {
                    0 => "No streams found. Is MiSTerKVMS Capture running on the capture PC, with a stream set up? \
                          You can also type its address."
                        .to_string(),
                    1 => "Found 1 stream.".to_string(),
                    n => format!("Found {n} streams."),
                });
                sync_pick();
                if stream.text().trim().is_empty() && count == 1 {
                    pick.set_selected(0);
                }
                glib::ControlFlow::Break
            });
        }
    };
    find.connect_clicked(move |_| search());
    find.emit_clicked();

    {
        let save = save.clone();
        name.connect_changed(move |e| save.set_sensitive(!e.text().trim().is_empty()));
    }
    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| {
            dialog.close();
        });
    }
    {
        let dialog = dialog.clone();
        save.connect_clicked(move |_| {
            let mut p = profile.clone();
            p.name = name.text().trim().to_string();
            p.source = "capture".into();
            p.stream = stream.text().trim().to_string();
            p.quality = QUALITIES.get(quality.selected() as usize).map_or("auto", |(value, _)| value).to_string();
            p.mister_host = mister_host.text().trim().to_string();
            p.mister_port = port.value() as u16;
            p.mouse_sensitivity = (sensitivity.value() * 100.0).round() / 100.0;
            p.alt_click_is_middle = alt_click.is_active();
            p.raw_mouse = raw_mouse.is_active();
            on_save(p, is_new);
            dialog.close();
        });
    }
    dialog.present(Some(parent));
}
