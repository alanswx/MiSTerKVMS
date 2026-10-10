//! Connect to a MiSTer, print state/status for a few seconds. Sends no input.
//! cargo run -p kvms-core --example probe [-- <mister-host>]
//! Defaults come from local.settings.json.
use std::sync::Arc;
#[path = "support/settings.rs"]
mod settings;

use kvms_core::{ConnectionState, MisterSession, SessionObserver};

struct Print;
impl SessionObserver for Print {
    fn on_state(&self, state: ConnectionState) { println!("state: {state:?}"); }
    fn on_status(&self, key: String, value: String) { println!("status: {key} = {value}"); }
}

fn main() {
    let settings = settings::Settings::load();
    let session = MisterSession::new(settings.mister_host(), settings.mister_port(), Arc::new(Print));
    std::thread::sleep(std::time::Duration::from_secs(2));
    println!("stats: {:?}", session.stats());
    session.disconnect();
    std::thread::sleep(std::time::Duration::from_millis(200));
}
