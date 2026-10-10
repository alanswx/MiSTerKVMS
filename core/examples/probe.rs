//! Connect to a MiSTer, print state/status for a few seconds. Sends no input.
//! cargo run -p kvms-core --example probe -- <mister-ip>
use std::sync::Arc;
use kvms_core::{ConnectionState, MisterSession, SessionObserver};

struct Print;
impl SessionObserver for Print {
    fn on_state(&self, state: ConnectionState) { println!("state: {state:?}"); }
    fn on_status(&self, key: String, value: String) { println!("status: {key} = {value}"); }
}

fn main() {
    let host = std::env::args().nth(1).unwrap_or_else(|| "<mister-ip>".into());
    let session = MisterSession::new(host, kvms_core::default_mrext_port(), Arc::new(Print));
    std::thread::sleep(std::time::Duration::from_secs(2));
    println!("stats: {:?}", session.stats());
    session.disconnect();
    std::thread::sleep(std::time::Duration::from_millis(200));
}
