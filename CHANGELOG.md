# AOOSTAR WTR MAX Screen Control Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

_Changes in the next release_

### Changed

- Replaced guest `RUN`/`STOP` text with graphical status indicators and aligned resource columns.

## v0.2.0-gem12.1 - 2026-09-05

### Added

- The `aster-pve` provider for Proxmox VE node, workload, storage, network, and host-time data.
- A single-page GEM12+ PVE dashboard with one-second refresh.
- Green/gray graphical network link indicators.
- MAFP touch input for wake and two-second hold-to-sleep gestures.
- A touch timing oscilloscope with partial LCD updates.
- Production LXC examples with stable device names, USB rebinding, and restricted SSH collection.

---

## v0.2.0 - 2025-08-31
### Fixed
- Misplaced text sensors in custom panels ([#11](https://github.com/zehnm/aoostar-rs/issues/11)).
- Wrong start position for circular progress (fan) sensor using a counter-clockwise direction ([#12](https://github.com/zehnm/aoostar-rs/issues/12)).
- aster-sysinfo tool: make sensor file world-readable, create all parent directories.

### Added
- Simple sensor panel with a file-based data source ([#6](https://github.com/zehnm/aoostar-rs/issues/6)).
- Initial support for fan-, progress-, & pointer-sensors ([#8](https://github.com/zehnm/aoostar-rs/pull/8)).
- Use [mdBook](https://rust-lang.github.io/mdBook/) for documentation and publish user guide to GitHub pages ([#10](https://github.com/zehnm/aoostar-rs/pull/10)).
- Initial `aster-sysinfo` tool for providing sensor values in a text file for `asterctl`.

### Changed
- Project structure using a Cargo workspace.

---

## v0.1.0 - 2025-08-02
### Added
- Initial `asterctl` tool release for controlling the LCD: on, off, display an image.
- systemd service file to switch off LCD on system start.
- Demo mode.
