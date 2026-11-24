//! `carrick-gui`: the Carrick window (see the library part, `carrick_gui`).
//!
//! The first process only looks after the window: it starts it in a second
//! process with one way of drawing, and if that one fails or crashes before
//! the window has drawn, it starts it again with the next. On Windows the
//! ways are DirectX 12, OpenGL, Vulkan and the software adapter every Windows
//! has (WARP); elsewhere, the graphics card through wgpu, OpenGL and the
//! software adapter (llvmpipe). The way that worked is remembered and tried
//! first next time. If none works it says why in a message, instead of
//! closing without a word, and everything is noted in `carrick-gui.log` in
//! the data folder. `CARRICK_RENDERER` picks one way by hand, in-process.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::SystemTime,
};

use carrick_gui::{App, Settings};
use eframe::{egui, egui_wgpu, wgpu};

/// Where the window notes how it draws, and why it could not.
static LOG: OnceLock<PathBuf> = OnceLock::new();
/// The way of drawing this process uses.
static MODE: OnceLock<String> = OnceLock::new();

/// How a try tells the first process that it could not draw the window.
const DRAW_FAILED: i32 = 3;

/// The ways to draw the window, most robust first.
#[cfg(windows)]
const WAYS: &[&str] = &["dx12", "glow", "vulkan", "software"];
#[cfg(not(windows))]
const WAYS: &[&str] = &["wgpu", "glow", "software"];

struct Window {
    app: App,
    frames: u32,
    remember: Option<PathBuf>,
}

impl eframe::App for Window {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.app.show(ui);
        // After a few frames the window has drawn: note the way that works,
        // so the first process knows, and starts with it next time.
        self.frames += 1;
        if self.frames == 3 {
            if let (Some(path), Some(mode)) = (self.remember.take(), MODE.get()) {
                let _ = std::fs::write(path, mode);
                if let Some(log) = LOG.get() {
                    write_log(log, &format!("{mode} draws the window"));
                }
            }
        }
    }
}

fn main() {
    let settings = match Settings::from_env() {
        Ok(settings) => settings,
        Err(e) => fail(&format!("Carrick cannot start: {e:#}"), None),
    };
    // The blob store builds its own runtime; this keeps it small too.
    if std::env::var_os("TOKIO_WORKER_THREADS").is_none() {
        std::env::set_var("TOKIO_WORKER_THREADS", settings.threads.to_string());
    }
    let root = settings.home.root().to_path_buf();
    let log = LOG.get_or_init(|| root.join("carrick-gui.log")).clone();
    let remembered = root.join("carrick-gui.renderer");

    let Ok(mode) = std::env::var("CARRICK_RENDERER") else {
        drop(settings);
        look_after(&log, &remembered);
    };
    // A try started by the first process, or a way chosen by hand.
    let a_try = std::env::var_os("CARRICK_RENDERER_AUTO").is_some();
    MODE.get_or_init(|| mode.clone());
    let remember = a_try.then(|| remembered.clone());
    if let Err(e) = run(settings, &mode, remember) {
        write_log(&log, &format!("{mode} could not draw the window: {e}"));
        if a_try {
            std::process::exit(DRAW_FAILED);
        }
        fail(&format!("Carrick could not open its window with {mode}: {e}"), Some(&log));
    }
}

/// The first process: starts the window with each way in turn until one
/// draws it, and waits for it to close.
fn look_after(log: &Path, remembered: &Path) -> ! {
    let mut ways: Vec<String> = WAYS.iter().map(|w| w.to_string()).collect();
    if let Ok(last) = std::fs::read_to_string(remembered) {
        if let Some(i) = ways.iter().position(|w| *w == last.trim()) {
            let way = ways.remove(i);
            ways.insert(0, way);
        }
    }
    let exe = std::env::current_exe().unwrap_or_else(|e| fail(&format!("Carrick cannot find itself: {e}"), Some(&log.to_path_buf())));
    let mut last = String::from("no way was tried");
    for way in &ways {
        let started = SystemTime::now();
        let status = std::process::Command::new(&exe)
            .args(std::env::args_os().skip(1))
            .env("CARRICK_RENDERER", way)
            .env("CARRICK_RENDERER_AUTO", "1")
            .status();
        let drew = std::fs::metadata(remembered)
            .and_then(|m| m.modified())
            .is_ok_and(|t| t >= started)
            && std::fs::read_to_string(remembered).is_ok_and(|w| w.trim() == way);
        match status {
            Ok(status) if status.success() => std::process::exit(0),
            // It worked and then stopped badly: say so, do not reopen it.
            Ok(status) if drew => {
                write_log(log, &format!("{way} drew the window, then stopped: {status}"));
                std::process::exit(status.code().unwrap_or(1));
            }
            Ok(status) if status.code() == Some(DRAW_FAILED) => last = format!("{way} could not draw it"),
            Ok(status) => {
                write_log(log, &format!("{way} stopped before drawing: {status}"));
                last = format!("{way} stopped ({status})");
            }
            Err(e) => {
                write_log(log, &format!("cannot start the window with {way}: {e}"));
                last = format!("cannot start it: {e}");
            }
        }
    }
    fail(
        &format!(
            "Carrick could not open its window: this computer gave it no way to draw it (the last try: {last}).\n\n\
             The terminal command, carrick, works without a window. Details are in {}.",
            log.display()
        ),
        Some(&log.to_path_buf()),
    );
}

fn run(settings: Settings, mode: &str, remember: Option<PathBuf>) -> eframe::Result {
    let mut wgpu_setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    wgpu_setup.instance_descriptor.backends = match mode {
        "dx12" => wgpu::Backends::DX12,
        "vulkan" => wgpu::Backends::VULKAN,
        // The software adapter, and any graphics card for "wgpu".
        _ => wgpu::Backends::all(),
    };
    wgpu_setup.native_adapter_selector = Some(Arc::new(pick_adapter));
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Carrick")
            .with_app_id("carrick")
            .with_inner_size([640.0, 720.0])
            .with_min_inner_size([480.0, 520.0])
            .with_drag_and_drop(true)
            .with_icon(icon()),
        renderer: if matches!(mode, "glow" | "opengl") { eframe::Renderer::Glow } else { eframe::Renderer::Wgpu },
        wgpu_options: egui_wgpu::WgpuConfiguration {
            wgpu_setup: egui_wgpu::WgpuSetup::CreateNew(wgpu_setup),
            ..Default::default()
        },
        ..Default::default()
    };
    eframe::run_native(
        "Carrick",
        options,
        Box::new(|cc| {
            carrick_gui::install(&cc.egui_ctx);
            Ok(Box::new(Window { app: App::new(settings), frames: 0, remember }))
        }),
    )
}

/// The best adapter that can draw on the window: a graphics card before a
/// software renderer, DirectX 12 or Metal before Vulkan, and OpenGL last. In
/// "software" mode, only a software one.
fn pick_adapter(adapters: &[wgpu::Adapter], surface: Option<&wgpu::Surface<'_>>) -> Result<wgpu::Adapter, String> {
    let software_only = MODE.get().is_some_and(|m| m == "software");
    let rank = |a: &wgpu::Adapter| {
        let info = a.get_info();
        let kind = match info.device_type {
            wgpu::DeviceType::DiscreteGpu => 0,
            wgpu::DeviceType::IntegratedGpu => 1,
            wgpu::DeviceType::VirtualGpu | wgpu::DeviceType::Other => 2,
            wgpu::DeviceType::Cpu => 3,
        };
        let backend = match info.backend {
            wgpu::Backend::Dx12 | wgpu::Backend::Metal => 0,
            wgpu::Backend::Vulkan => 1,
            _ => 2,
        };
        (kind, backend)
    };
    let mut usable: Vec<&wgpu::Adapter> = adapters
        .iter()
        .filter(|a| !software_only || a.get_info().device_type == wgpu::DeviceType::Cpu)
        .filter(|a| surface.is_none_or(|s| a.is_surface_supported(s)))
        .collect();
    usable.sort_by_key(|a| rank(a));
    if let (Some(a), Some(log)) = (usable.first(), LOG.get()) {
        let info = a.get_info();
        let mode = MODE.get().map(String::as_str).unwrap_or("wgpu");
        write_log(log, &format!("{mode}: trying {} ({:?}, {:?})", info.name, info.backend, info.device_type));
    }
    usable.first().map(|a| (*a).clone()).ok_or_else(|| {
        let seen: Vec<String> = adapters.iter().map(|a| format!("{} ({:?})", a.get_info().name, a.get_info().backend)).collect();
        if seen.is_empty() {
            "no graphics adapter for it".to_string()
        } else {
            format!("no adapter can draw on the window; found {}", seen.join(", "))
        }
    })
}

fn write_log(path: &Path, line: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{} {line}", carrick_core::home::now_secs());
    }
}

/// Says why the window cannot open, where a windowed program can be heard.
fn fail(text: &str, log: Option<&PathBuf>) -> ! {
    eprintln!("{text}");
    if let Some(log) = log {
        write_log(log, text);
    }
    rfd::MessageDialog::new().set_title("Carrick").set_description(text).set_level(rfd::MessageLevel::Error).show();
    std::process::exit(1);
}
/// The window icon: two arrows tied in one knot, 64 x 64, made from
/// packaging/icon-source.png by packaging/icon.py.
fn icon() -> egui::IconData {
    const N: u32 = 64;
    let rgba = include_bytes!("icon-64.rgba").to_vec();
    debug_assert_eq!(rgba.len(), (N * N * 4) as usize);
    egui::IconData { rgba, width: N, height: N }
}
