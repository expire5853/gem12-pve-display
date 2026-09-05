# GEM12+ PVE Status Display

[简体中文](README.md) | [English](README.en.md)

A Linux display-control toolkit based on
[zehnm/aoostar-rs](https://github.com/zehnm/aoostar-rs), focused on the AOOSTAR
GEM12+ and Proxmox VE. It continuously presents node, guest, storage, and
network status on the built-in 960×376 secondary display and can use touch
events from the MAFP fingerprint module as screen controls.

![GEM12+ single-page Proxmox VE dashboard](docs/img/pve-dashboard.png)

> The address and workloads shown above are documentation examples and do not
> represent a real environment.

## Features

- A single-page PVE dashboard for node, CPU, memory, load, uptime, storage, and guest status.
- One-second refresh with the time reported by the PVE host.
- Green filled and gray hollow indicators for network links, plus the primary interface IP/CIDR.
- Touch input from the GEM12+ MAFP fingerprint module without fingerprint enrollment or matching:
  - Any touch wakes the display while it is off.
  - Holding for two seconds switches the display off.
- A touch timing oscilloscope that displays press, release, and minimum detectable intervals.
- Production assets for a dedicated unprivileged LXC, persistent USB rebinding, and restricted SSH collection.
- The image display, dynamic sensor panels, partial updates, and display power controls inherited from `aoostar-rs`.

## Included tools

| Program | Purpose |
| --- | --- |
| `asterctl` | Controls the display, renders images and live panels, and handles touch gestures |
| `aster-pve` | Collects PVE node, guest, storage, network, and host-time data over SSH |
| `aster-sysinfo` | Exports Linux sensor data as text values consumed by `asterctl` |
| `fingerprint-scope` | Shows MAFP press and release states on a cyclic on-screen timeline |
| `fingerprint-touch` | Prints raw MAFP touch events for diagnostics |

## Quick start

Download the Linux x64 bundle from
[Releases](https://github.com/expire5853/gem12-pve-display/releases), or build it locally:

```shell
cargo build --release --bins --all-features
```

Collect a PVE snapshot and print it to the terminal:

```shell
aster-pve --host root@pve.example.com --storage local-lvm --console
```

Preview the panel with a simulated serial port and sanitized sample data. This
does not access a physical display:

```shell
asterctl \
  --simulate --save \
  --config pve-monitor.json \
  --config-dir cfg \
  --font-dir fonts \
  --sensor-path docs/examples/pve-sensors.txt
```

Rendered images are written to `out/`. See the
[PVE status panel guide](docs/pve.md) for operation and the
[dedicated LXC deployment guide](deploy/README.md) for production setup.

## General sensor panels

In addition to the PVE-specific dashboard, this fork remains compatible with
the original AOOSTAR-X dynamic panel configuration:

![AOOSTAR dynamic sensor panel](docs/img/sensor_panel-02.png)

The shared LCD protocol and base tools are documented in the upstream
[User Guide](https://zehnm.github.io/aoostar-rs).

## Safety notice

The display protocol was reverse-engineered from AOOSTAR-X and has no official
manufacturer documentation. Before using it, be aware that:

- It may not work with every firmware version or hardware revision.
- Unexpected commands may leave the display firmware unresponsive and require a power cycle.
- This project uses the fingerprint module only as a touch input; it does not inspect, enroll, or match fingerprints.
- Do not run `fprintd` or another fingerprint client that owns the same USB interface at the same time.

You use this software at your own risk.

## Contributing

Issues and pull requests are welcome. For substantial protocol or deployment
changes, please open an issue first to discuss the approach.

## License

Licensed under either of the following, at your option:

- [Apache License 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)
