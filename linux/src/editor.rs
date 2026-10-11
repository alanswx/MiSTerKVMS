//! Add or edit a host: the MiSTer, its NDI source, mouse options.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use kvms_core::profiles::HostProfile;

pub fn show(parent: &impl IsA<gtk::Widget>, profile: HostProfile, is_new: bool, on_save: impl Fn(HostProfile, bool) + 'static) {
    let name = adw::EntryRow::builder().title("Name").text(profile.name.as_str()).build();
    let mister_host = adw::EntryRow::builder().title("MiSTer IP or hostname").text(profile.mister_host.as_str()).build();
    let port = adw::SpinRow::with_range(1.0, 65535.0, 1.0);
    port.set_title("mrext port");
    port.set_value(profile.mister_port as f64);
    let mister = adw::PreferencesGroup::builder().title("MiSTer").description("Keyboard and mouse, through mrext Remote").build();
    mister.add(&mister_host);
    mister.add(&port);

    let obs_host = adw::EntryRow::builder().title("OBS PC (optional)").text(profile.obs_host.as_str()).build();
    let ndi_source = adw::EntryRow::builder().title("NDI source").text(profile.ndi_source.as_str()).build();
    let found = gtk::StringList::new(&[]);
    let pick = gtk::DropDown::builder().model(&found).valign(gtk::Align::Center).build();
    pick.set_tooltip_text(Some("Sources found on the network and on the OBS PC above"));
    pick.set_visible(false);
    let find = gtk::Button::builder().label("Find").valign(gtk::Align::Center).build();
    let spinner = gtk::Spinner::builder().spinning(true).build();
    spinner.set_visible(false);
    ndi_source.add_suffix(&spinner);
    ndi_source.add_suffix(&pick);
    ndi_source.add_suffix(&find);
    let video = adw::PreferencesGroup::builder()
        .title("Video and audio")
        .description(
            "From OBS over NDI®. Enter the OBS PC to list its sources directly; otherwise sources found on the \
             local network are listed. NDI discovery needs avahi-daemon running. See ndi.video.",
        )
        .build();
    video.add(&obs_host);
    video.add(&ndi_source);

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
    page.add(&mister);
    page.add(&video);
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

    // NDI discovery blocks for a couple of seconds: run it on a thread; a
    // search that finishes after a newer one started is ignored.
    let search_id = Rc::new(Cell::new(0u32));
    let search = {
        let (obs_host, ndi_source, found, pick, find, spinner, search_id) =
            (obs_host.clone(), ndi_source.clone(), found.clone(), pick.clone(), find.clone(), spinner.clone(), search_id.clone());
        move || {
            let id = search_id.get() + 1;
            search_id.set(id);
            let hosts = obs_host.text().trim().to_string();
            spinner.set_visible(true);
            find.set_sensitive(false);
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(kvms_core::video::ndi_find_sources(hosts, 2000).map_err(|e| e.to_string()));
            });
            let (ndi_source, found, pick, find, spinner, search_id) =
                (ndi_source.clone(), found.clone(), pick.clone(), find.clone(), spinner.clone(), search_id.clone());
            glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                let Ok(result) = rx.try_recv() else { return glib::ControlFlow::Continue };
                if id != search_id.get() {
                    return glib::ControlFlow::Break;
                }
                spinner.set_visible(false);
                find.set_sensitive(true);
                let names = result.unwrap_or_else(|e| {
                    kvms_core::log(&format!("ndi: {e}"));
                    Vec::new()
                });
                found.splice(0, found.n_items(), &names.iter().map(String::as_str).collect::<Vec<_>>());
                pick.set_visible(!names.is_empty());
                if ndi_source.text().is_empty() && names.len() == 1 {
                    ndi_source.set_text(&names[0]);
                }
                glib::ControlFlow::Break
            });
        }
    };
    let search = Rc::new(search);
    {
        let search = search.clone();
        find.connect_clicked(move |_| search());
    }
    {
        let search = search.clone();
        obs_host.connect_apply(move |_| search());
    }
    obs_host.set_show_apply_button(true);
    {
        let ndi_source = ndi_source.clone();
        pick.connect_selected_item_notify(move |pick| {
            if let Some(item) = pick.selected_item().and_then(|i| i.downcast::<gtk::StringObject>().ok()) {
                ndi_source.set_text(&item.string());
            }
        });
    }
    search();

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
            p.mister_host = mister_host.text().trim().to_string();
            p.mister_port = port.value() as u16;
            p.obs_host = obs_host.text().trim().to_string();
            p.ndi_source = ndi_source.text().trim().to_string();
            p.mouse_sensitivity = (sensitivity.value() * 100.0).round() / 100.0;
            p.alt_click_is_middle = alt_click.is_active();
            p.raw_mouse = raw_mouse.is_active();
            on_save(p, is_new);
            dialog.close();
        });
    }
    dialog.present(Some(parent));
}
