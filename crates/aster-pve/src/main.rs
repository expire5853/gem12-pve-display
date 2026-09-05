// SPDX-License-Identifier: MIT OR Apache-2.0

#![forbid(non_ascii_idents)]
#![deny(unsafe_code)]

use anyhow::{Context, anyhow, bail};
use clap::Parser;
use env_logger::Env;
use log::{debug, info};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufWriter, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};
use tempfile::Builder;

const NODE_MARKER: &str = "__ASTER_NODE__";
const TIME_MARKER: &str = "__ASTER_TIME__";
const STATUS_MARKER: &str = "__ASTER_STATUS__";
const GUESTS_MARKER: &str = "__ASTER_GUESTS__";
const STORAGE_MARKER: &str = "__ASTER_STORAGE__";
const NETWORK_MARKER: &str = "__ASTER_NETWORK__";

const REMOTE_COMMAND: &str = r#"set -eu
node=$(hostname)
task_dir=$(mktemp -d /tmp/aster-pve.XXXXXX)
cleanup() { rm -rf -- "$task_dir"; }
trap cleanup EXIT HUP INT TERM

pvesh get "/nodes/$node/status" --output-format json > "$task_dir/status" &
status_pid=$!
pvesh get /cluster/resources --type vm --output-format json > "$task_dir/guests" &
guests_pid=$!
pvesh get "/nodes/$node/storage" --output-format json > "$task_dir/storage" &
storage_pid=$!
pvesh get "/nodes/$node/network" --output-format json > "$task_dir/network" &
network_pid=$!
wait "$status_pid" "$guests_pid" "$storage_pid" "$network_pid"

printf '__ASTER_NODE__%s\n' "$node"
printf '__ASTER_TIME__'
date '+%H:%M:%S'
printf '__ASTER_STATUS__'
cat "$task_dir/status"
printf '\n__ASTER_GUESTS__'
cat "$task_dir/guests"
printf '\n__ASTER_STORAGE__'
cat "$task_dir/storage"
printf '\n__ASTER_NETWORK__'
cat "$task_dir/network"
printf '\n'
"#;

/// Gather Proxmox VE node status over SSH for an asterctl sensor panel.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// SSH destination of the PVE host, for example root@pve.example.com.
    #[arg(long)]
    host: String,

    /// Optional SSH private key.
    #[arg(short = 'i', long)]
    identity: Option<PathBuf>,

    /// Prefer this PVE storage ID in the display. The largest active storage is used otherwise.
    #[arg(long)]
    storage: Option<String>,

    /// Output sensor file.
    #[arg(short, long)]
    out: Option<PathBuf>,

    /// Temporary directory used for atomic sensor-file updates.
    #[arg(short, long)]
    temp_dir: Option<PathBuf>,

    /// Print values to stdout.
    #[arg(long)]
    console: bool,

    /// Refresh interval in seconds. Run once when omitted or zero.
    #[arg(short, long, default_value_t = 0)]
    refresh: u16,

    /// SSH connection timeout in seconds.
    #[arg(long, default_value_t = 5)]
    connect_timeout: u16,
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();
    let args = Args::parse();

    if args.host.starts_with('-') || args.host.trim().is_empty() {
        bail!("--host must be a non-empty SSH destination and cannot start with '-'");
    }
    if args.out.is_none() && !args.console {
        bail!("specify --out, --console, or both");
    }
    if let Some(out) = &args.out
        && let Some(parent) = out.parent()
    {
        fs::create_dir_all(parent)?;
    }

    let refresh = Duration::from_secs(args.refresh.into());
    if !refresh.is_zero() {
        info!("collecting PVE status every {} seconds", args.refresh);
    }

    loop {
        let started = Instant::now();
        let raw = fetch_over_ssh(&args)?;
        let sensors = parse_snapshot(&raw, args.storage.as_deref())?;

        if let Some(out) = &args.out {
            write_sensor_file(out, args.temp_dir.as_deref(), &sensors)?;
        }
        if args.console {
            for (key, value) in &sensors {
                println!("{key}: {value}");
            }
            println!();
        }

        if refresh.is_zero() {
            break;
        }
        if let Some(remaining) = refresh.checked_sub(started.elapsed()) {
            sleep(remaining);
        }
    }

    Ok(())
}

fn fetch_over_ssh(args: &Args) -> anyhow::Result<String> {
    let mut command = Command::new("ssh");
    command
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg(format!("ConnectTimeout={}", args.connect_timeout));
    if let Some(identity) = &args.identity {
        command.arg("-i").arg(identity);
    }
    command.arg("--").arg(&args.host).arg(REMOTE_COMMAND);

    debug!("fetching PVE status from {}", args.host);
    let output = command.output().context("failed to execute ssh")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        bail!("PVE status command failed ({}): {stderr}", output.status);
    }
    String::from_utf8(output.stdout).context("SSH output is not UTF-8")
}

fn parse_snapshot(
    raw: &str,
    preferred_storage: Option<&str>,
) -> anyhow::Result<BTreeMap<String, String>> {
    let node = marker_value(raw, NODE_MARKER)?.trim();
    let host_time = marker_value(raw, TIME_MARKER)?.trim();
    let status: Value = serde_json::from_str(marker_value(raw, STATUS_MARKER)?)
        .context("invalid node status JSON")?;
    let guests: Value = serde_json::from_str(marker_value(raw, GUESTS_MARKER)?)
        .context("invalid guest resource JSON")?;
    let storages: Value =
        serde_json::from_str(marker_value(raw, STORAGE_MARKER)?).context("invalid storage JSON")?;
    let network: Value =
        serde_json::from_str(marker_value(raw, NETWORK_MARKER)?).context("invalid network JSON")?;

    let mut sensors = BTreeMap::new();
    sensors.insert("pve_node".into(), node.into());
    sensors.insert("pve_node_display".into(), format!("PVE · {node}"));
    sensors.insert("pve_time_display".into(), host_time.into());

    let raw_version = status["pveversion"].as_str().unwrap_or("unknown");
    let version = raw_version
        .strip_prefix("pve-manager/")
        .unwrap_or(raw_version)
        .split('/')
        .next()
        .unwrap_or("unknown");
    let kernel = status["current-kernel"]["release"]
        .as_str()
        .unwrap_or("unknown");
    sensors.insert("pve_version".into(), version.into());
    sensors.insert("pve_version_short_display".into(), format!("PVE {version}"));
    sensors.insert("pve_kernel".into(), kernel.into());
    sensors.insert(
        "pve_version_display".into(),
        format!("PVE {version}  ·  Linux {kernel}"),
    );

    let cpu_percent = number(&status["cpu"], "status.cpu")? * 100.0;
    sensors.insert("pve_cpu_percent".into(), format!("{cpu_percent:.1}"));
    sensors.insert("pve_cpu_percent#unit".into(), "%".into());
    sensors.insert("pve_cpu_display".into(), format!("CPU  {cpu_percent:.1}%"));

    add_usage(&mut sensors, "memory", "RAM", &status["memory"])?;
    add_usage(&mut sensors, "rootfs", "ROOT", &status["rootfs"])?;

    let load = status["loadavg"]
        .as_array()
        .and_then(|values| values.first())
        .and_then(Value::as_str)
        .unwrap_or("?");
    sensors.insert("pve_load_one".into(), load.into());
    sensors.insert("pve_load_display".into(), format!("LOAD  {load}"));

    let uptime = integer(&status["uptime"], "status.uptime")?;
    sensors.insert("pve_uptime_seconds".into(), uptime.to_string());
    sensors.insert(
        "pve_uptime_display".into(),
        format!("UP  {}", format_uptime(uptime)),
    );

    add_guest_counts(&mut sensors, &guests)?;
    add_storage(&mut sensors, &storages, preferred_storage)?;
    add_network(&mut sensors, &network)?;

    Ok(sensors)
}

fn add_network(sensors: &mut BTreeMap<String, String>, network: &Value) -> anyhow::Result<()> {
    let interfaces = network
        .as_array()
        .ok_or_else(|| anyhow!("network is not an array"))?;
    let primary = interfaces
        .iter()
        .find(|interface| interface["gateway"].is_string() && interface["address"].is_string())
        .or_else(|| {
            interfaces
                .iter()
                .find(|interface| interface["iface"].as_str() == Some("vmbr0"))
        })
        .ok_or_else(|| anyhow!("no primary PVE network interface found"))?;

    let iface = primary["iface"].as_str().unwrap_or("network");
    let address = primary["cidr"]
        .as_str()
        .or_else(|| primary["address"].as_str())
        .unwrap_or("no-ip");
    let status = if interface_is_up(primary) {
        "UP"
    } else {
        "DOWN"
    };
    sensors.insert("pve_network_iface".into(), iface.into());
    sensors.insert("pve_network_status".into(), status.into());
    sensors.insert("pve_ip".into(), address.into());
    sensors.insert(
        "pve_network_online_dot".into(),
        online_dot(interface_is_up(primary)).into(),
    );
    sensors.insert(
        "pve_network_offline_dot".into(),
        offline_dot(interface_is_up(primary)).into(),
    );
    sensors.insert("pve_network_label".into(), format!("{iface}  {address}"));

    for name in ["nic0", "nic1", "wlp4s0"] {
        let online = interfaces
            .iter()
            .find(|interface| interface["iface"].as_str() == Some(name))
            .is_some_and(interface_is_up);
        sensors.insert(format!("pve_{name}_online_dot"), online_dot(online).into());
        sensors.insert(
            format!("pve_{name}_offline_dot"),
            offline_dot(online).into(),
        );
        sensors.insert(format!("pve_{name}_label"), name.into());
    }

    let mut link_states = interfaces
        .iter()
        .filter_map(|interface| {
            let name = interface["iface"].as_str()?;
            (interface["type"].as_str() == Some("eth") && interface["exists"].as_u64() == Some(1))
                .then(|| {
                    format!(
                        "{name} {}",
                        if interface_is_up(interface) {
                            "UP"
                        } else {
                            "DOWN"
                        }
                    )
                })
        })
        .collect::<Vec<_>>();
    link_states.sort();
    link_states.truncate(3);

    let mut network_display = format!("{iface} {status} · {address}");
    for link in link_states {
        network_display.push_str(" · ");
        network_display.push_str(&link);
    }
    sensors.insert("pve_network_display".into(), network_display.clone());
    let version = sensors
        .get("pve_version_display")
        .cloned()
        .unwrap_or_else(|| "PVE".into());
    sensors.insert(
        "pve_status_display".into(),
        format!("{version}  ·  {network_display}"),
    );
    Ok(())
}

fn interface_is_up(interface: &Value) -> bool {
    interface["active"].as_u64() == Some(1)
}

fn online_dot(online: bool) -> &'static str {
    if online { "●" } else { " " }
}

fn offline_dot(online: bool) -> &'static str {
    if online { " " } else { "○" }
}

fn marker_value<'a>(raw: &'a str, marker: &str) -> anyhow::Result<&'a str> {
    raw.lines()
        .find_map(|line| line.strip_prefix(marker))
        .ok_or_else(|| anyhow!("missing {marker} in SSH output"))
}

fn add_usage(
    sensors: &mut BTreeMap<String, String>,
    key: &str,
    display_name: &str,
    value: &Value,
) -> anyhow::Result<()> {
    let used = integer(&value["used"], &format!("status.{key}.used"))?;
    let total = integer(&value["total"], &format!("status.{key}.total"))?;
    let percent = percent(used, total);
    sensors.insert(format!("pve_{key}_used_bytes"), used.to_string());
    sensors.insert(format!("pve_{key}_total_bytes"), total.to_string());
    sensors.insert(format!("pve_{key}_percent"), format!("{percent:.1}"));
    sensors.insert(
        format!("pve_{key}_display"),
        format!("{display_name}  {:.1}/{:.1} GiB", gib(used), gib(total)),
    );
    Ok(())
}

fn add_guest_counts(sensors: &mut BTreeMap<String, String>, guests: &Value) -> anyhow::Result<()> {
    let guests = guests
        .as_array()
        .ok_or_else(|| anyhow!("guests is not an array"))?;
    let mut total = 0_u64;
    let mut running = 0_u64;
    let mut lxc_total = 0_u64;
    let mut lxc_running = 0_u64;
    let mut qemu_total = 0_u64;
    let mut qemu_running = 0_u64;

    for guest in guests {
        total += 1;
        let is_running = guest["status"].as_str() == Some("running");
        running += u64::from(is_running);
        match guest["type"].as_str() {
            Some("lxc") => {
                lxc_total += 1;
                lxc_running += u64::from(is_running);
            }
            Some("qemu") => {
                qemu_total += 1;
                qemu_running += u64::from(is_running);
            }
            _ => {}
        }
    }

    for (key, value) in [
        ("pve_guests_total", total),
        ("pve_guests_running", running),
        ("pve_lxc_total", lxc_total),
        ("pve_lxc_running", lxc_running),
        ("pve_qemu_total", qemu_total),
        ("pve_qemu_running", qemu_running),
    ] {
        sensors.insert(key.into(), value.to_string());
    }
    let all_running = total > 0 && running == total;
    sensors.insert(
        "pve_guests_online_dot".into(),
        if all_running { "●" } else { " " }.into(),
    );
    sensors.insert(
        "pve_guests_offline_dot".into(),
        if total > 0 && !all_running {
            "○"
        } else {
            " "
        }
        .into(),
    );
    sensors.insert(
        "pve_guests_display".into(),
        format!("GUESTS  {running}/{total}"),
    );
    sensors.insert(
        "pve_guest_list_title".into(),
        format!("WORKLOADS  ·  {running}/{total} RUNNING"),
    );
    sensors.insert(
        "pve_guest_summary".into(),
        format!("{lxc_running}/{lxc_total} LXC  ·  {qemu_running}/{qemu_total} VM"),
    );

    let mut sorted = guests.iter().collect::<Vec<_>>();
    sorted.sort_by_key(|guest| guest["vmid"].as_u64().unwrap_or_default());
    for index in 0..5 {
        let prefix = format!("pve_guest_{index}");
        let Some(guest) = sorted.get(index) else {
            for suffix in [
                "online_dot",
                "offline_dot",
                "identity_display",
                "cpu_display",
                "memory_display",
                "display",
            ] {
                sensors.insert(format!("{prefix}_{suffix}"), " ".into());
            }
            continue;
        };

        let vmid = guest["vmid"].as_u64().unwrap_or_default();
        let name = guest["name"].as_str().unwrap_or("unnamed");
        let name = name.chars().take(18).collect::<String>();
        let running = guest["status"].as_str() == Some("running");
        let cpu = guest["cpu"].as_f64().unwrap_or_default() * 100.0;
        let mem = guest["mem"].as_u64().unwrap_or_default();
        let max_mem = guest["maxmem"].as_u64().unwrap_or_default();
        let identity = format!("{vmid:>3}  {name}");
        let cpu_display = format!("CPU {cpu:>4.1}%");
        let memory_display = format!("RAM {:>4.1}/{:.1}G", gib(mem), gib(max_mem));

        sensors.insert(format!("{prefix}_online_dot"), online_dot(running).into());
        sensors.insert(format!("{prefix}_offline_dot"), offline_dot(running).into());
        sensors.insert(format!("{prefix}_identity_display"), identity.clone());
        sensors.insert(format!("{prefix}_cpu_display"), cpu_display.clone());
        sensors.insert(format!("{prefix}_memory_display"), memory_display.clone());
        sensors.insert(
            format!("{prefix}_display"),
            format!(
                "{}  {identity:<23}  {cpu_display}  {memory_display}",
                if running { "●" } else { "○" }
            ),
        );
    }
    Ok(())
}

fn add_storage(
    sensors: &mut BTreeMap<String, String>,
    storages: &Value,
    preferred: Option<&str>,
) -> anyhow::Result<()> {
    let storages = storages
        .as_array()
        .ok_or_else(|| anyhow!("storages is not an array"))?;
    let active = storages.iter().filter(|storage| {
        storage["active"].as_u64() == Some(1) && storage["enabled"].as_u64() == Some(1)
    });
    let storage = if let Some(preferred) = preferred {
        active
            .clone()
            .find(|storage| storage["storage"].as_str() == Some(preferred))
            .ok_or_else(|| anyhow!("preferred storage '{preferred}' is not active"))?
    } else {
        active
            .max_by_key(|storage| storage["total"].as_u64().unwrap_or_default())
            .ok_or_else(|| anyhow!("no active PVE storage found"))?
    };

    let name = storage["storage"].as_str().unwrap_or("storage");
    let used = integer(&storage["used"], "storage.used")?;
    let total = integer(&storage["total"], "storage.total")?;
    sensors.insert("pve_storage_name".into(), name.into());
    sensors.insert("pve_storage_used_bytes".into(), used.to_string());
    sensors.insert("pve_storage_total_bytes".into(), total.to_string());
    sensors.insert(
        "pve_storage_percent".into(),
        format!("{:.1}", percent(used, total)),
    );
    sensors.insert(
        "pve_storage_display".into(),
        format!("{name}  {:.1}/{:.1} GiB", gib(used), gib(total)),
    );
    Ok(())
}

fn number(value: &Value, name: &str) -> anyhow::Result<f64> {
    value
        .as_f64()
        .ok_or_else(|| anyhow!("{name} is not a number"))
}

fn integer(value: &Value, name: &str) -> anyhow::Result<u64> {
    value
        .as_u64()
        .ok_or_else(|| anyhow!("{name} is not an unsigned integer"))
}

fn percent(used: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        used as f64 * 100.0 / total as f64
    }
}

fn gib(bytes: u64) -> f64 {
    bytes as f64 / 1024.0 / 1024.0 / 1024.0
}

fn format_uptime(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = seconds % 86_400 / 3_600;
    let minutes = seconds % 3_600 / 60;
    if days > 0 {
        format!("{days}d {hours:02}h")
    } else {
        format!("{hours:02}h {minutes:02}m")
    }
}

fn write_sensor_file(
    out_file: &Path,
    temp_dir: Option<&Path>,
    sensors: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    if out_file.is_dir() {
        bail!("output cannot be a directory: {}", out_file.display());
    }
    let permissions = fs::Permissions::from_mode(0o664);
    let tmp = if let Some(dir) = temp_dir {
        fs::create_dir_all(dir)?;
        Builder::new().permissions(permissions).tempfile_in(dir)?
    } else {
        Builder::new().permissions(permissions).tempfile()?
    };
    let mut writer = BufWriter::new(&tmp);
    for (key, value) in sensors {
        writeln!(writer, "{key}: {value}")?;
    }
    writer.flush()?;
    drop(writer);
    tmp.persist(out_file)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to replace {}", out_file.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = concat!(
        "banner\n",
        "__ASTER_NODE__pve\n",
        "__ASTER_TIME__10:09:53\n",
        "__ASTER_STATUS__{\"cpu\":0.125,\"pveversion\":\"pve-manager/9.2.2/hash\",",
        "\"current-kernel\":{\"release\":\"7.0.2-6-pve\"},\"loadavg\":[\"0.42\"],",
        "\"memory\":{\"used\":17179869184,\"total\":68719476736},",
        "\"rootfs\":{\"used\":5368709120,\"total\":42949672960},\"uptime\":90061}\n",
        "__ASTER_GUESTS__[{\"vmid\":100,\"name\":\"router\",\"type\":\"lxc\",",
        "\"status\":\"running\",\"cpu\":0.012,\"mem\":858993459,",
        "\"maxmem\":2147483648},{\"vmid\":101,\"name\":\"backup\",",
        "\"type\":\"qemu\",\"status\":\"stopped\",\"cpu\":0.0,\"mem\":0,",
        "\"maxmem\":4294967296}]\n",
        "__ASTER_STORAGE__[{\"storage\":\"local\",\"active\":1,\"enabled\":1,",
        "\"used\":10737418240,\"total\":107374182400}]\n",
        "__ASTER_NETWORK__[{\"iface\":\"nic0\",\"type\":\"eth\",\"exists\":1,",
        "\"active\":0},{\"iface\":\"nic1\",\"type\":\"eth\",\"exists\":1,",
        "\"active\":1},{\"iface\":\"vmbr0\",\"type\":\"bridge\",\"active\":1,",
        "\"address\":\"192.0.2.10\",\"cidr\":\"192.0.2.10/24\",",
        "\"gateway\":\"192.0.2.1\"}]\n"
    );

    #[test]
    fn parses_pve_snapshot() {
        let values = parse_snapshot(SAMPLE, Some("local")).unwrap();
        assert_eq!(values["pve_node"], "pve");
        assert_eq!(values["pve_time_display"], "10:09:53");
        assert_eq!(values["pve_version"], "9.2.2");
        assert_eq!(values["pve_cpu_percent"], "12.5");
        assert_eq!(values["pve_memory_percent"], "25.0");
        assert_eq!(values["pve_guests_display"], "GUESTS  1/2");
        assert_eq!(values["pve_guests_online_dot"], " ");
        assert_eq!(values["pve_guests_offline_dot"], "○");
        assert_eq!(values["pve_guest_0_online_dot"], "●");
        assert_eq!(values["pve_guest_0_offline_dot"], " ");
        assert_eq!(values["pve_guest_0_identity_display"], "100  router");
        assert_eq!(values["pve_guest_0_cpu_display"], "CPU  1.2%");
        assert_eq!(values["pve_guest_0_memory_display"], "RAM  0.8/2.0G");
        assert_eq!(values["pve_guest_1_online_dot"], " ");
        assert_eq!(values["pve_guest_1_offline_dot"], "○");
        assert_eq!(values["pve_guest_1_identity_display"], "101  backup");
        assert_eq!(values["pve_storage_display"], "local  10.0/100.0 GiB");
        assert_eq!(values["pve_uptime_display"], "UP  1d 01h");
        assert_eq!(values["pve_network_iface"], "vmbr0");
        assert_eq!(values["pve_network_status"], "UP");
        assert_eq!(values["pve_ip"], "192.0.2.10/24");
        assert_eq!(values["pve_network_online_dot"], "●");
        assert_eq!(values["pve_network_offline_dot"], " ");
        assert_eq!(values["pve_nic0_online_dot"], " ");
        assert_eq!(values["pve_nic0_offline_dot"], "○");
        assert_eq!(values["pve_nic1_online_dot"], "●");
        assert_eq!(
            values["pve_network_display"],
            "vmbr0 UP · 192.0.2.10/24 · nic0 DOWN · nic1 UP"
        );
    }

    #[test]
    fn chooses_largest_active_storage() {
        let values = parse_snapshot(SAMPLE, None).unwrap();
        assert_eq!(values["pve_storage_name"], "local");
    }

    #[test]
    fn rejects_missing_preferred_storage() {
        assert!(parse_snapshot(SAMPLE, Some("missing")).is_err());
    }
}
