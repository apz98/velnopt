//! Print the host's network, Bluetooth, audio, and battery state. Read-only.

use std::{thread, time::Duration};

use velnopt_services::{Enabled, Services};

fn main() -> std::io::Result<()> {
    let all = Enabled {
        network: true,
        bluetooth: true,
        audio: true,
        battery: true,
        // the tray would serve a watcher on the session bus, which isn't read-only
        tray: false,
    };
    let (services, _source) = Services::new(all)?;
    thread::sleep(Duration::from_millis(1500));
    println!("{:#?}", services.snapshot());
    Ok(())
}
