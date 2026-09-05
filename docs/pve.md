# Proxmox VE status panel

`aster-pve` collects node, guest, and storage status using the PVE host's
`pvesh` command over SSH. This is useful when `asterctl` runs inside an LXC
container that owns the LCD serial device.

The SSH account needs permission to run these read-only commands:

- `pvesh get /nodes/<node>/status`
- `pvesh get /cluster/resources --type vm`
- `pvesh get /nodes/<node>/storage`

## Development setup

Collect a snapshot and inspect it:

```shell
aster-pve --host root@pve.example.com --storage local-lvm --console
```

Run the provider continuously:

```shell
install -d /run/asterctl/sensors
aster-pve \
  --host root@pve.example.com \
  --storage local-lvm \
  --out /run/asterctl/sensors/pve.txt \
  --temp-dir /run/asterctl/sensors \
  --refresh 1
```

In another process, start the panel renderer:

```shell
asterctl \
  --device /dev/ttyACM0 \
  --config pve-monitor.json \
  --config-dir cfg \
  --font-dir fonts \
  --sensor-path /run/asterctl/sensors \
  --fingerprint 3274:8012
```

With fingerprint touch control enabled:

- Any touch wakes the display while it is off. The release that follows is ignored.
- Holding the sensor for 2 seconds switches the display off.

The PVE configuration combines node resources and up to four workload rows into one page. Timed rotation
is disabled, and double taps are ignored when only one panel is active. The hold threshold can be
changed with `--fingerprint-long-press-ms`.

The network row uses a green filled circle for an online interface and a gray hollow circle for an
offline interface. The primary bridge also shows its CIDR address.

Guest rows use the same graphical convention: a green filled circle means the guest is running and
a gray hollow circle means it is stopped. Identity, CPU, and memory values occupy fixed columns. The
summary circle is green only when every listed guest is running; otherwise it is gray and hollow.

The touch-only protocol does not inspect, enroll, identify, or delete fingerprints. Do not run
`fprintd` or another fingerprint client at the same time because the USB interface is exclusive.

## Touch timing scope

Stop `asterctl` first, then run the LCD oscilloscope to diagnose the sensor's press and release
timing:

```shell
fingerprint-scope --device /dev/ttyACM0 --fingerprint 3274:8012
```

The trace uses a cyclic sweep and cached partial LCD updates. A press is drawn at the high level
and a release at the low level. The lower area shows hold time, physical up-gap, press-to-press
period, release-to-release period, and whether the same up-gap double-tap rule used by `asterctl`
matched.
If two fast physical taps appear as one continuous high pulse, the sensor firmware merged them;
if there is only one pulse, the second hardware interrupt was not delivered.

The defaults are a 40 ms trace resolution, 125 ms batched LCD updates, an approximately 12 second
sweep, a 1000 ms double-tap threshold, and a 2000 ms long-press threshold. They can be changed with
`--sample-ms`, `--display-ms`, `--sweep-seconds`, `--double-tap-min-ms`, `--double-tap-ms`, and
`--long-press-ms`.
Event timestamps are captured in the USB worker, so LCD transfer latency does not alter the measured
press, release, or double-tap intervals.

Use `--simulate --save` instead of `--device /dev/ttyACM0` to render PNG files
without writing to the physical display.

Only one process may own the LCD serial device at a time.

For a dedicated production LXC, including persistent USB mapping and a restricted
SSH key, see the [deployment assets](../deploy/README.md).
