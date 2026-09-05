// SPDX-License-Identifier: MIT OR Apache-2.0

use asterctl::fingerprint::{parse_usb_id, start_touch_listener};
use clap::Parser;
use std::time::Duration;

/// Print press/release events from a Microarray MAFP fingerprint sensor.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Fingerprint USB device ID in vid:pid notation.
    #[arg(long, default_value = "3274:8012")]
    fingerprint: String,

    /// Presence polling interval in milliseconds.
    #[arg(long, default_value_t = 30)]
    poll_ms: u64,
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let args = Args::parse();
    let (vid, pid) = parse_usb_id(&args.fingerprint)?;
    let events = start_touch_listener(vid, pid, Duration::from_millis(args.poll_ms));
    for event in events {
        println!("{event:?}");
    }
    Ok(())
}
