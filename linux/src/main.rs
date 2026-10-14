//! MiSTerKVMS for Linux: a GTK 4 + libadwaita window on the shared core.
//! The core does the work (sessions, video, stats, recording); this file
//! and its modules show its state and capture keyboard and mouse.

#[cfg(target_os = "linux")]
mod about;
#[cfg(target_os = "linux")]
mod capture;
#[cfg(target_os = "linux")]
mod editor;
#[cfg(target_os = "linux")]
mod video;
#[cfg(target_os = "linux")]
mod window;

#[cfg(target_os = "linux")]
fn main() -> gtk::glib::ExitCode {
    use adw::prelude::*;

    kvms_core::log(&format!("--- MiSTerKVMS {} started", env!("CARGO_PKG_VERSION")));
    let app = adw::Application::builder().application_id(window::APP_ID).build();
    app.connect_startup(|_| {
        // The icon ships next to the binary (tarball) or in the source tree
        // (cargo run); installed copies use the system icon theme.
        let display = gtk::gdk::Display::default().expect("display");
        let theme = gtk::IconTheme::for_display(&display);
        let next_to_exe = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("icons")));
        for dir in next_to_exe.into_iter().chain([std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/data/icons"))]) {
            if dir.exists() {
                theme.add_search_path(dir);
            }
        }
        gtk::Window::set_default_icon_name(window::APP_ID);
    });
    app.connect_activate(|app| {
        // One window: activating again (e.g. from the launcher) shows it.
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }
        window::Window::new(app).present();
    });
    app.run()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("The GTK app is for Linux; on this system use the macOS or Windows app.");
    std::process::exit(1);
}
