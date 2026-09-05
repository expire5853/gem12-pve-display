# Production LXC deployment

This directory contains example assets for a dedicated, unprivileged Debian LXC
that owns the GEM12+ LCD and MAFP touch sensor. Replace all example values before
installing them.

## Suggested container profile

- Debian 13
- 1 vCPU, 512 MiB RAM, 256 MiB swap, 8 GiB root disk
- DHCP on `vmbr0`
- Unprivileged, nesting enabled, PVE autostart enabled

Choose an unused container ID and create `/etc/default/gem12-display` on both the
PVE host and inside the LXC from `gem12-display.env.example`:

```ini
PVE_HOST=root@pve.example.com
GEM12_CT_ID=103
```

## Host files

Install these on the PVE host:

- `99-gem12.rules` as `/etc/udev/rules.d/99-gem12.rules`
- `gem12-usb-rebind` as `/usr/local/sbin/gem12-usb-rebind`
- `gem12-usb-rebind.service` as `/etc/systemd/system/gem12-usb-rebind.service`
- `aster-pve-snapshot` as `/usr/local/sbin/aster-pve-snapshot`

Reload udev and systemd after installation. The udev rule gives the LCD a stable
name and triggers the rebind service whenever the fingerprint sensor receives a
new USB bus address. The rebind service updates `dev1` for `GEM12_CT_ID` and
reboots a running display container only when the address actually changed.

## Container files

Install release binaries and assets inside the LXC:

```text
/opt/gem12/bin/asterctl
/opt/gem12/bin/aster-pve
/etc/gem12/pve-monitor.json
/etc/gem12/fonts/DejaVuSans.ttf
/etc/systemd/system/gem12-pve.service
/etc/systemd/system/gem12-display.service
/etc/default/gem12-display
```

Enable both services:

```shell
systemctl daemon-reload
systemctl enable --now gem12-pve.service gem12-display.service
```

`gem12-pve.service` collects PVE status once per second. `gem12-display.service`
renders it on the LCD and uses the MAFP sensor only as a touch input. Do not run
another `asterctl` process against the same LCD.

## Restrict the PVE SSH key

Generate a dedicated SSH key inside the LXC. Add only its public key to the PVE
root account and force the read-only snapshot command:

```text
restrict,command="/usr/local/sbin/aster-pve-snapshot" ssh-ed25519 AAAA... gem12-display
```

With this restriction, SSH requests made with the display key always execute the
snapshot helper and cannot open a general PVE root shell. Test the restriction
before enabling the collector service.
