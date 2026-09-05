// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025 Markus Zehnder

#![forbid(non_ascii_idents)]
#![deny(unsafe_code)]

use asterctl::cfg::{MonitorConfig, Panel, load_custom_panel};
use asterctl::fingerprint::{TimedTouchEvent, parse_usb_id, start_timed_touch_listener};
use asterctl::gesture::{GestureAction, GestureController};
use asterctl::render::PanelRenderer;
use asterctl::sensors::{read_filter_file, read_key_value_file, start_file_slurper};
use asterctl::{cfg, img};
use asterctl_lcd::{AooScreen, AooScreenBuilder, DISPLAY_SIZE};

use anyhow::anyhow;
use clap::Parser;
use env_logger::Env;
use log::{debug, error, info};
use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, RwLock};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// AOOSTAR WTR MAX and GEM12+ PRO screen control.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Serial device, for example, "/dev/cu.usbserial-AB0KOHLS". Takes priority over --usb option.
    #[arg(short, long)]
    device: Option<String>,

    /// USB serial UART "vid:pid" in hex notation (lsusb output). Default: 416:90A1
    #[arg(short, long)]
    usb: Option<String>,

    /// Switch display on and exit. This will show the last displayed image.
    #[arg(long)]
    on: bool,

    /// Switch display off and exit.
    #[arg(long)]
    off: bool,

    /// Image to display, other sizes than 960x376 will be scaled.
    #[arg(short, long)]
    image: Option<String>,

    /// AOOSTAR-X json configuration file to parse.
    ///
    /// The configuration file will be loaded from the `config_dir` directory if no full path is
    /// specified.
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Include one or more additional custom panels into the base configuration.
    ///
    /// Specify the path to the panel directory containing panel.json and fonts / img subdirectories.
    #[arg(short, long)]
    panels: Option<Vec<PathBuf>>,

    /// Configuration directory containing configuration files and background images
    /// specified in the `config` file.
    #[arg(long, default_value_t = String::from("cfg"))]
    config_dir: String, // default_value_t requires Display trait which PathBuf does not implement

    /// Font directory for fonts specified in the `config` file.
    #[arg(long, default_value_t = String::from("fonts"))]
    font_dir: String,

    /// Single sensor value input file or directory for multiple sensor input files.
    #[arg(long, default_value_t = String::from("cfg/sensors"))]
    sensor_path: String,

    /// Sensor identifier mapping file. Ignored if the file does not exist.
    ///
    /// The configuration file will be loaded from the `config_dir` directory if no full path is
    /// specified.
    #[arg(long, default_value_t = String::from("sensor-mapping.cfg"))]
    sensor_mapping: String,

    /// Switch off display n seconds after loading image or running demo.
    #[arg(short, long)]
    off_after: Option<u32>,

    /// Test mode: only write to the display without checking response.
    #[arg(short, long)]
    write_only: bool,

    /// Test mode: save changed images in ./out folder.
    #[arg(short, long)]
    save: bool,

    /// Simulate serial port for testing and development, `--device` and `--usb` options are ignored.
    #[arg(long)]
    simulate: bool,

    /// Use a MAFP fingerprint sensor as a touch control, specified as USB "vid:pid".
    #[arg(long, value_name = "VID:PID")]
    fingerprint: Option<String>,

    /// Duration in milliseconds that closes the display when held.
    #[arg(long, default_value_t = 2000)]
    fingerprint_long_press_ms: u64,

    /// Maximum gap in milliseconds between two taps that switches panel.
    #[arg(long, default_value_t = 1000)]
    fingerprint_double_tap_ms: u64,

    /// Minimum released gap in milliseconds between two taps that switches panel.
    #[arg(long, default_value_t = 150)]
    fingerprint_double_tap_min_ms: u64,
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();

    let args = Args::parse();
    if args.fingerprint_double_tap_min_ms > args.fingerprint_double_tap_ms {
        return Err(anyhow!(
            "fingerprint double-tap minimum must not exceed the maximum"
        ));
    }

    // initialize display with given UART port parameter
    let mut builder = AooScreenBuilder::new();
    builder.no_init_check(args.write_only);
    let mut screen = if args.simulate {
        builder.simulate()?
    } else if let Some(device) = args.device {
        builder.open_device(&device)?
    } else if let Some(usb) = args.usb {
        builder.open_usb_id(&usb)?
    } else {
        builder.open_default()?
    };

    // process simple commands
    if args.off {
        screen.off()?;
        return Ok(());
    } else if args.on {
        screen.on()?;
        return Ok(());
    }

    // switch on screen for remaining commands
    screen.init()?;

    if let Some(config) = args.config {
        info!("Starting sensor panel mode");
        let img_save_path = if args.save {
            let img_save_path = PathBuf::from("out");
            fs::create_dir_all(&img_save_path)?;
            Some(img_save_path)
        } else {
            None
        };

        let cfg_dir = PathBuf::from(args.config_dir);
        let font_dir = PathBuf::from(args.font_dir);
        let sensor_path = PathBuf::from(args.sensor_path);
        let mapping_cfg = PathBuf::from(args.sensor_mapping);
        let cfg = load_configuration(&config, &cfg_dir, args.panels, &mapping_cfg)?;
        let touch_events = if let Some(id) = args.fingerprint.as_deref() {
            let (vid, pid) = parse_usb_id(id)?;
            Some(start_timed_touch_listener(
                vid,
                pid,
                Duration::from_millis(30),
            ))
        } else {
            None
        };
        let runtime = PanelRuntime {
            cfg_dir,
            font_dir,
            sensor_path,
            img_save_path,
            touch_events,
            long_press: Duration::from_millis(args.fingerprint_long_press_ms),
            double_tap_min: Duration::from_millis(args.fingerprint_double_tap_min_ms),
            double_tap_max: Duration::from_millis(args.fingerprint_double_tap_ms),
        };
        run_sensor_panel(&mut screen, cfg, runtime)?;
        return Ok(());
    }

    if let Some(image) = args.image {
        info!("Loading and displaying background image {image}...");
        let rgb_img = img::load_image(&image, Some(DISPLAY_SIZE))?.to_rgb8();
        let timestamp = Instant::now();
        screen.send_image(&rgb_img)?;
        debug!("Image sent in {}ms", timestamp.elapsed().as_millis());
    }

    if let Some(off) = args.off_after {
        info!("Switching off display in {off}s");
        sleep(Duration::from_secs(off as u64));
        screen.off()?;
    }

    info!("Bye bye!");

    Ok(())
}

fn load_configuration<P: AsRef<Path>>(
    config: P,
    config_dir: P,
    panels: Option<Vec<PathBuf>>,
    sensor_mapping: P,
) -> anyhow::Result<MonitorConfig> {
    let config = config.as_ref();
    let config_dir = config_dir.as_ref();

    let mut cfg = if config.is_absolute() {
        cfg::load_cfg(config)?
    } else {
        cfg::load_cfg(config_dir.join(config))?
    };

    if let Some(panels) = panels {
        for panel in panels {
            cfg.include_custom_panel(load_custom_panel(panel)?);
        }
    }

    let sensor_mapping = sensor_mapping.as_ref();
    let mapping_cfg = if sensor_mapping.is_absolute() {
        sensor_mapping.to_path_buf()
    } else {
        config_dir.join(sensor_mapping)
    };
    if mapping_cfg.is_file() {
        let mut mapping = HashMap::new();
        read_key_value_file(&mapping_cfg, &mut mapping, None)?;
        cfg.set_sensor_mapping(mapping);
    } else {
        info!("Sensor mapping file {mapping_cfg:?} not found");
    }

    cfg.sensor_filter = load_sensor_filter(&mapping_cfg)?;

    Ok(cfg)
}

fn load_sensor_filter(mapping_cfg: &Path) -> anyhow::Result<Option<Vec<Regex>>> {
    if let Some(parent) = mapping_cfg.parent()
        && let Some(file_stem) = mapping_cfg.file_stem()
        && let Some(extension) = mapping_cfg.extension()
    {
        let filter_file = parent
            .join(format!("{}-filter", file_stem.to_string_lossy()))
            .with_extension(extension);

        if filter_file.is_file() {
            info!("Loading sensor filter file {filter_file:?}");
            return read_filter_file(filter_file);
        } else {
            info!("No sensor filter file {filter_file:?} available");
        }
    }

    Ok(None)
}

struct PanelRuntime {
    cfg_dir: PathBuf,
    font_dir: PathBuf,
    sensor_path: PathBuf,
    img_save_path: Option<PathBuf>,
    touch_events: Option<Receiver<TimedTouchEvent>>,
    long_press: Duration,
    double_tap_min: Duration,
    double_tap_max: Duration,
}

fn run_sensor_panel(
    screen: &mut AooScreen,
    mut cfg: MonitorConfig,
    runtime: PanelRuntime,
) -> anyhow::Result<()> {
    let PanelRuntime {
        cfg_dir,
        font_dir,
        sensor_path,
        img_save_path,
        touch_events,
        long_press,
        double_tap_min,
        double_tap_max,
    } = runtime;

    let mut renderer = PanelRenderer::new(DISPLAY_SIZE, &font_dir, &cfg_dir);
    if let Some(img_save_path) = &img_save_path {
        renderer.set_img_save_path(img_save_path);
        renderer.set_save_render_img(true);
        // renderer.set_save_processed_pic(true);
        // renderer.set_save_progress_layer(true);
    }

    let sensor_values: Arc<RwLock<HashMap<String, String>>> = Arc::new(RwLock::new(HashMap::new()));

    start_file_slurper(
        sensor_path,
        sensor_values.clone(),
        cfg.sensor_filter.clone(),
    )?;

    let refresh = Duration::from_millis((cfg.setup.refresh * 1000f32) as u64);

    let switch_time = cfg
        .setup
        .switch_time
        .as_deref()
        .and_then(|v| f32::from_str(v).ok())
        .map(|v| (v > 0.0).then(|| Duration::from_millis((v * 1000.0) as u64)))
        .unwrap_or(Some(Duration::from_secs(5)));

    let mut gestures = GestureController::new(long_press, double_tap_min, double_tap_max);
    let mut screen_on = true;
    let panel_switch_enabled = cfg
        .active_panels
        .iter()
        .filter(|panel| **panel > 0 && **panel <= cfg.panels.len() as u32)
        .count()
        > 1;

    // Panel switching loop. Fingerprint events are polled frequently enough for responsive
    // gesture timing while rendering remains controlled by the panel refresh interval.
    loop {
        let panel = cfg
            .get_next_active_panel()
            .ok_or(anyhow!("No active panel"))?;

        info!("Switching panel: {}", panel.friendly_name());
        let mut panel_switch_time = Instant::now();
        let mut next_refresh = Instant::now();

        // active panel refresh loop
        let mut refresh_count = 1;
        loop {
            let now = Instant::now();
            if let Some(action) = gestures.tick(screen_on, now) {
                apply_gesture(action, screen, &mut screen_on)?;
            }

            let mut next_panel = false;
            if let Some(events) = &touch_events {
                while let Ok(event) = events.try_recv() {
                    if let Some(action) = gestures.handle(event.event, screen_on, event.at) {
                        if action == GestureAction::NextPanel && !panel_switch_enabled {
                            debug!("Fingerprint double tap ignored: only one active panel");
                            continue;
                        }
                        next_panel = action == GestureAction::NextPanel;
                        apply_gesture(action, screen, &mut screen_on)?;
                        if action == GestureAction::Wake {
                            next_refresh = Instant::now();
                            panel_switch_time = Instant::now();
                        }
                    }
                }
            }
            if next_panel {
                break;
            }

            let now = Instant::now();
            if screen_on && now >= next_refresh {
                if img_save_path.is_some() {
                    renderer.set_img_suffix(format!("-{refresh_count:02}"));
                }

                // Keeping the read lock during rendering avoids cloning the sensor map.
                let values = sensor_values.read().expect("RwLock is poisoned");
                update_panel(screen, &mut renderer, panel, &values)?;
                drop(values);

                refresh_count += 1;
                next_refresh = Instant::now() + refresh;
            }

            if screen_on
                && switch_time.is_some_and(|switch_time| panel_switch_time.elapsed() >= switch_time)
            {
                break;
            }

            sleep(Duration::from_millis(20));
        }
    }
}

fn apply_gesture(
    action: GestureAction,
    screen: &mut AooScreen,
    screen_on: &mut bool,
) -> anyhow::Result<()> {
    match action {
        GestureAction::Wake => {
            info!("Fingerprint touch: waking display");
            screen.on()?;
            *screen_on = true;
        }
        GestureAction::NextPanel => info!("Fingerprint double tap: switching panel"),
        GestureAction::Sleep => {
            info!("Fingerprint long press: switching display off");
            screen.off()?;
            *screen_on = false;
        }
    }
    Ok(())
}

fn update_panel(
    screen: &mut AooScreen,
    renderer: &mut PanelRenderer,
    panel: &Panel,
    values: &HashMap<String, String>,
) -> anyhow::Result<()> {
    debug!("Displaying panel '{}'...", panel.friendly_name());

    match renderer.render(panel, values) {
        Ok(image) => screen.send_image(&image)?,
        Err(e) => error!("Error rendering panel '{}': {e:?}", panel.friendly_name()),
    }

    Ok(())
}
