//! Minimal CLI client for gglinuxd — connects, requests the device list, and prints
//! every message (responses + live events) until the read times out.
//!
//! Run the daemon, then: `cargo run -p gglinuxd --example ggctl`

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

fn main() -> std::io::Result<()> {
    let path = gg_ipc::socket_path();
    let mut stream = UnixStream::connect(&path).map_err(|e| {
        eprintln!("could not connect to {} — is gglinuxd running? ({e})", path.display());
        e
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;

    // Ask for the device list (the daemon also pushes it on connect).
    stream.write_all(b"{\"id\":1,\"req\":{\"method\":\"list_devices\"}}\n")?;

    let reader = BufReader::new(stream);
    for line in reader.lines() {
        match line {
            Ok(l) => println!("{l}"),
            Err(_) => break, // read timeout — nothing more to read
        }
    }
    Ok(())
}
