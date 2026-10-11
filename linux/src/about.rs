//! About: version and credits (the same as the macOS and Windows apps').

use adw::prelude::*;

pub fn show(parent: &impl IsA<gtk::Widget>) {
    let about = adw::AboutDialog::builder()
        .application_name("MiSTerKVMS")
        .application_icon(crate::window::APP_ID)
        .version(env!("CARGO_PKG_VERSION"))
        .comments("Keyboard, mouse, video and sound for your MiSTer FPGAs, over the network.")
        .website("https://github.com/danifunker/MiSTerKVMS")
        .issue_url("https://github.com/danifunker/MiSTerKVMS/issues")
        .license_type(gtk::License::MitX11)
        .build();
    about.add_credit_section(
        Some("MiSTer-Kun mascot"),
        &["Original design by HeWhoisRed, a gift to the MiSTer community", "Remastered artwork: baxysquare https://github.com/baxysquare/mister_kun"],
    );
    about.add_acknowledgement_section(
        Some("Built on"),
        &[
            "MiSTer FPGA by Sorgelig and contributors https://github.com/MiSTer-devel/Main_MiSTer/wiki",
            "mrext Remote by wizzo and contributors https://github.com/wizzomafizzo/mrext",
            "OBS Studio https://obsproject.com",
            "DistroAV https://github.com/DistroAV/DistroAV",
            "NDI® https://ndi.video",
        ],
    );
    about.add_legal_section(
        "NDI®",
        None,
        gtk::License::Custom,
        Some("NDI® is a registered trademark of Vizrt NDI AB. See <a href=\"https://ndi.video\">ndi.video</a>."),
    );
    about.present(Some(parent));
}
