// SPDX-License-Identifier: MIT OR Apache-2.0

//! Minimal touch-only support for the Microarray MAFP MOC sensor.
//!
//! This intentionally does not read, enroll, match, or delete fingerprints. It only polls the
//! sensor's image-presence result and translates it to press/release events.

use anyhow::{Context, anyhow, bail};
use log::{debug, error, info, warn};
use rusb::{DeviceHandle, Direction, GlobalContext, Recipient, RequestType, request_type};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::sleep;
use std::time::{Duration, Instant};

const BULK_OUT: u8 = 0x03;
const BULK_IN: u8 = 0x83;
const INTERRUPT_IN: u8 = 0x82;
const INTERFACE: u8 = 0;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const CLEAN_TIMEOUT: Duration = Duration::from_millis(20);
const INTERRUPT_TIMEOUT: Duration = Duration::from_secs(1);
const CONTROL_TIMEOUT: Duration = Duration::from_millis(200);

const PACKET_COMMAND: u8 = 0x01;
const PACKET_ANSWER: u8 = 0x07;
const COMMAND_HANDSHAKE: u8 = 0x35;
const COMMAND_INIT_STATUS: u8 = 0x88;
const COMMAND_GET_IMAGE: u8 = 0x01;
const COMMAND_SLEEP: u8 = 0x33;
const RESULT_FINGER_PRESENT: u8 = 0x00;
const RESULT_NO_FINGER: u8 = 0x02;
const SLEEP_INTERRUPT_WAIT: u8 = 0;
const SLEEP_INTERRUPT_CHECK: u8 = 1;
const REQUEST_INTERRUPT_ENABLE: u8 = 0x89;
const INTERRUPT_TOUCH: [u8; 2] = [0x04, 0xe5];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TouchEvent {
    Pressed,
    Released,
}

#[derive(Clone, Copy, Debug)]
pub struct TimedTouchEvent {
    pub event: TouchEvent,
    pub at: Instant,
}

/// Start a reconnecting background listener for a MAFP touch sensor.
pub fn start_touch_listener(vid: u16, pid: u16, poll_interval: Duration) -> Receiver<TouchEvent> {
    let timed_events = start_timed_touch_listener(vid, pid, poll_interval);
    let (sender, receiver) = channel();
    std::thread::spawn(move || {
        for event in timed_events {
            if sender.send(event.event).is_err() {
                break;
            }
        }
    });
    receiver
}

/// Start a listener whose timestamps are captured in the USB worker at the detection edge.
pub fn start_timed_touch_listener(
    vid: u16,
    pid: u16,
    poll_interval: Duration,
) -> Receiver<TimedTouchEvent> {
    let (sender, receiver) = channel();
    std::thread::spawn(move || {
        loop {
            match run_device(vid, pid, poll_interval, &sender) {
                Ok(()) => return,
                Err(error) => {
                    error!("fingerprint touch listener failed: {error:#}");
                    sleep(Duration::from_secs(2));
                }
            }
        }
    });
    receiver
}

fn run_device(
    vid: u16,
    pid: u16,
    poll_interval: Duration,
    sender: &Sender<TimedTouchEvent>,
) -> anyhow::Result<()> {
    let mut handle = rusb::open_device_with_vid_pid(vid, pid)
        .ok_or_else(|| anyhow!("USB fingerprint sensor {vid:04x}:{pid:04x} not found"))?;
    handle
        .reset()
        .context("failed to reset fingerprint sensor")?;
    handle
        .claim_interface(INTERFACE)
        .context("failed to claim fingerprint sensor interface")?;
    drain_bulk_input(&handle);

    let handshake = send_command(&mut handle, COMMAND_HANDSHAKE, &[])?;
    if handshake.first() != Some(&0)
        || handshake.get(1) != Some(&b'M')
        || handshake.get(2) != Some(&b'A')
    {
        bail!("unexpected fingerprint handshake response: {handshake:02x?}");
    }
    // Some firmware revisions time out on this query. A successful handshake is sufficient.
    if let Err(error) = send_command(&mut handle, COMMAND_INIT_STATUS, &[]) {
        debug!("fingerprint init-status query ignored: {error:#}");
        drain_bulk_input(&handle);
    }

    info!("fingerprint touch sensor {vid:04x}:{pid:04x} ready");
    emit(sender, TouchEvent::Released)?;

    loop {
        wait_for_finger(&mut handle)?;
        if image_has_finger(&mut handle)? {
            emit(sender, TouchEvent::Pressed)?;
            wait_for_release(&mut handle, poll_interval)?;
            emit(sender, TouchEvent::Released)?;
        }
    }
}

fn wait_for_finger(handle: &mut DeviceHandle<GlobalContext>) -> anyhow::Result<()> {
    // The MAFP firmware stops actively scanning after reporting no finger. Match libfprint's
    // wake-up sequence: configure interrupt detection, enable it through the vendor control
    // request, and wait on the interrupt endpoint before asking for an image again.
    ensure_success(
        COMMAND_SLEEP,
        &send_command(handle, COMMAND_SLEEP, &[SLEEP_INTERRUPT_CHECK])?,
    )?;
    ensure_success(
        COMMAND_SLEEP,
        &send_command(handle, COMMAND_SLEEP, &[SLEEP_INTERRUPT_WAIT])?,
    )?;
    set_interrupt(handle, true)?;

    let mut interrupt = [0_u8; 2];
    loop {
        match handle.read_interrupt(INTERRUPT_IN, &mut interrupt, INTERRUPT_TIMEOUT) {
            Ok(2) if interrupt == INTERRUPT_TOUCH => break,
            Ok(length) => debug!(
                "ignoring fingerprint interrupt (length {length}): {:02x?}",
                &interrupt[..length.min(interrupt.len())]
            ),
            Err(rusb::Error::Timeout) => continue,
            Err(error) => return Err(error).context("failed to read fingerprint touch interrupt"),
        }
    }

    set_interrupt(handle, false)
}

fn wait_for_release(
    handle: &mut DeviceHandle<GlobalContext>,
    poll_interval: Duration,
) -> anyhow::Result<()> {
    loop {
        if !image_has_finger(handle)? {
            return Ok(());
        }
        sleep(poll_interval);
    }
}

fn image_has_finger(handle: &mut DeviceHandle<GlobalContext>) -> anyhow::Result<bool> {
    match send_command(handle, COMMAND_GET_IMAGE, &[])?
        .first()
        .copied()
    {
        Some(RESULT_FINGER_PRESENT) => Ok(true),
        Some(RESULT_NO_FINGER) => Ok(false),
        Some(result) => {
            warn!("unexpected fingerprint presence result 0x{result:02x}");
            Ok(false)
        }
        None => bail!("empty fingerprint presence response"),
    }
}

fn set_interrupt(handle: &DeviceHandle<GlobalContext>, enabled: bool) -> anyhow::Result<()> {
    let mut response = [0_u8; 1];
    handle
        .read_control(
            request_type(Direction::In, RequestType::Vendor, Recipient::Device),
            REQUEST_INTERRUPT_ENABLE,
            u16::from(enabled),
            0,
            &mut response,
            CONTROL_TIMEOUT,
        )
        .with_context(|| {
            format!(
                "failed to {} fingerprint touch interrupt",
                if enabled { "enable" } else { "disable" }
            )
        })?;
    Ok(())
}

fn ensure_success(command: u8, response: &[u8]) -> anyhow::Result<()> {
    match response.first() {
        Some(&0) => Ok(()),
        Some(result) => bail!("fingerprint command 0x{command:02x} failed with 0x{result:02x}"),
        None => bail!("empty fingerprint command 0x{command:02x} response"),
    }
}

fn emit(sender: &Sender<TimedTouchEvent>, event: TouchEvent) -> anyhow::Result<()> {
    sender
        .send(TimedTouchEvent {
            event,
            at: Instant::now(),
        })
        .map_err(|_| anyhow!("fingerprint touch receiver closed"))?;
    debug!("fingerprint touch event: {event:?}");
    Ok(())
}

fn drain_bulk_input(handle: &DeviceHandle<GlobalContext>) {
    let mut buffer = [0_u8; 512];
    while handle
        .read_bulk(BULK_IN, &mut buffer, CLEAN_TIMEOUT)
        .is_ok()
    {}
}

fn send_command(
    handle: &mut DeviceHandle<GlobalContext>,
    command: u8,
    data: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let packet = build_command(command, data)?;
    let written = handle
        .write_bulk(BULK_OUT, &packet, COMMAND_TIMEOUT)
        .with_context(|| format!("failed to send fingerprint command 0x{command:02x}"))?;
    if written != packet.len() {
        bail!(
            "short fingerprint command write: {written}/{} bytes",
            packet.len()
        );
    }

    let mut response = Vec::with_capacity(512);
    loop {
        let mut chunk = [0_u8; 512];
        let read = handle
            .read_bulk(BULK_IN, &mut chunk, COMMAND_TIMEOUT)
            .with_context(|| format!("failed to read fingerprint command 0x{command:02x}"))?;
        response.extend_from_slice(&chunk[..read]);

        if response.len() >= 9 {
            let frame_len = u16::from_be_bytes([response[7], response[8]]) as usize;
            let packet_len = 9 + frame_len;
            if response.len() >= packet_len {
                response.truncate(packet_len);
                return parse_answer(&response);
            }
        }
    }
}

fn build_command(command: u8, data: &[u8]) -> anyhow::Result<Vec<u8>> {
    let frame_len = 1_usize + data.len() + 2;
    let frame_len = u16::try_from(frame_len).context("fingerprint command is too large")?;
    let mut packet = Vec::with_capacity(9 + frame_len as usize);
    packet.extend_from_slice(&[0xef, 0x01, 0xff, 0xff, 0xff, 0xff, PACKET_COMMAND]);
    packet.extend_from_slice(&frame_len.to_be_bytes());
    packet.push(command);
    packet.extend_from_slice(data);
    let checksum = checksum(&packet);
    packet.extend_from_slice(&checksum.to_be_bytes());
    Ok(packet)
}

fn parse_answer(packet: &[u8]) -> anyhow::Result<Vec<u8>> {
    if packet.len() < 12 || packet[..6] != [0xef, 0x01, 0xff, 0xff, 0xff, 0xff] {
        bail!("invalid fingerprint response header");
    }
    if packet[6] != PACKET_ANSWER {
        bail!("unexpected fingerprint response type 0x{:02x}", packet[6]);
    }
    let frame_len = u16::from_be_bytes([packet[7], packet[8]]) as usize;
    if packet.len() != 9 + frame_len || frame_len < 3 {
        bail!("invalid fingerprint response length");
    }
    let checksum_offset = packet.len() - 2;
    let expected = u16::from_be_bytes([packet[checksum_offset], packet[checksum_offset + 1]]);
    let actual = checksum(&packet[..checksum_offset]);
    if actual != expected {
        bail!("fingerprint response checksum mismatch: {actual:04x} != {expected:04x}");
    }
    Ok(packet[9..checksum_offset].to_vec())
}

fn checksum(packet_without_checksum: &[u8]) -> u16 {
    packet_without_checksum[6..]
        .iter()
        .fold(0_u16, |sum, byte| sum.wrapping_add(u16::from(*byte)))
}

pub fn parse_usb_id(id: &str) -> anyhow::Result<(u16, u16)> {
    let (vid, pid) = id
        .split_once(':')
        .ok_or_else(|| anyhow!("expected fingerprint USB ID in vid:pid format"))?;
    Ok((u16::from_str_radix(vid, 16)?, u16::from_str_radix(pid, 16)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_handshake_packet() {
        assert_eq!(
            build_command(COMMAND_HANDSHAKE, &[]).unwrap(),
            [
                0xef, 0x01, 0xff, 0xff, 0xff, 0xff, 0x01, 0x00, 0x03, 0x35, 0x00, 0x39
            ]
        );
    }

    #[test]
    fn parses_answer_packet() {
        let packet = [
            0xef, 0x01, 0xff, 0xff, 0xff, 0xff, 0x07, 0x00, 0x05, 0x00, b'M', b'A', 0x00, 0x9a,
        ];
        assert_eq!(parse_answer(&packet).unwrap(), [0, b'M', b'A']);
    }

    #[test]
    fn parses_usb_identifier() {
        assert_eq!(parse_usb_id("3274:8012").unwrap(), (0x3274, 0x8012));
        assert!(parse_usb_id("invalid").is_err());
    }
}
