//! Helpers for the window tests: windows that are never shown on screen
//! (egui's test harness), driven through their accessibility tree, as a
//! screen reader would, and read back from what they painted.
#![allow(dead_code)]

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use eframe::egui::{self, Role, Shape};
use egui_kittest::{
    kittest::{by, Queryable},
    Harness,
};
use carrick_core::{
    home::Home,
    net::{NetOptions, RelayChoice},
};
use carrick_gui::{App, Settings};

pub type Window = Harness<'static, App>;

/// How long a test waits for a transfer before giving up.
pub const WAIT: Duration = Duration::from_secs(180);

/// No relay and no lookup service: endpoints find each other by the
/// addresses in a ticket, so the tests need no Internet.
pub fn offline() -> NetOptions {
    NetOptions { relay: RelayChoice::Disabled, ..Default::default() }
}

/// A window with its own data folder, as if it ran on its own computer.
pub fn window(home: &Path, net: NetOptions) -> Window {
    let settings = Settings {
        home: Home::at(home).unwrap(),
        net,
        notify: false,
        default_dest: None,
        threads: 2,
    };
    Harness::builder()
        .with_size(egui::vec2(640.0, 760.0))
        .with_context_setup(carrick_gui::install)
        .build_ui_state(|ui, app: &mut App| app.show(ui), App::new(settings))
}

pub fn settle(w: &mut Window) {
    w.run_steps(4);
}

/// Presses the control with this role and name.
pub fn press(w: &mut Window, role: Role, name: &str) {
    w.get_by_role_and_label(role, name).click_accesskit();
    settle(w);
}

pub fn button(w: &mut Window, name: &str) {
    press(w, Role::Button, name);
}

pub fn tab(w: &mut Window, name: &str) {
    press(w, Role::Tab, name);
}

pub fn tick(w: &mut Window, name: &str) {
    press(w, Role::CheckBox, name);
}

/// Types into the text field with this name.
pub fn type_into(w: &mut Window, name: &str, text: &str) {
    w.get(by().label(name).role(Role::TextInput)).focus();
    w.step();
    w.get(by().label(name).role(Role::TextInput)).type_text(text);
    settle(w);
}

/// Drops files on the window, as from a file manager.
pub fn drop_on(w: &mut Window, paths: &[PathBuf]) {
    for path in paths {
        w.input_mut().dropped_files.push(Arc::new(Dropped(path.clone())));
    }
    settle(w);
}

#[derive(Debug)]
struct Dropped(PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

/// Presses the button and returns what it put on the clipboard.
pub fn copy_with(w: &mut Window, name: &str) -> String {
    w.get_by_role_and_label(Role::Button, name).click_accesskit();
    for _ in 0..8 {
        w.step();
        for command in &w.output().platform_output.commands {
            if let egui::OutputCommand::CopyText(text) = command {
                return text.clone();
            }
        }
    }
    panic!("{name} put nothing on the clipboard");
}

/// Steps the window until `done` holds, while the transfers run.
pub fn wait(w: &mut Window, what: &str, done: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done(w.state()) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        w.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    settle(w);
}

/// Steps two windows together until `done` holds for the second one.
pub fn wait_both(a: &mut Window, b: &mut Window, what: &str, done: impl Fn(&App, &App) -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done(a.state(), b.state()) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        a.step();
        b.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    settle(a);
    settle(b);
}

/// Every text the window painted in its last frame.
pub fn painted(w: &Window) -> Vec<String> {
    fn walk(s: &Shape, out: &mut Vec<String>) {
        match s {
            Shape::Text(t) => out.push(t.galley.text().to_owned()),
            Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for clipped in &w.output().shapes {
        walk(&clipped.shape, &mut out);
    }
    out
}

pub fn shows(w: &Window, text: &str) -> bool {
    painted(w).iter().any(|p| p.contains(text))
}

/// Repeatable bytes that do not compress.
pub fn fill(path: &Path, size: usize, seed: u64) {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut data = Vec::with_capacity(size + 8);
    while data.len() < size {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        data.extend_from_slice(&state.to_le_bytes());
    }
    data.truncate(size);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, data).unwrap();
}

/// A small folder with nested files, an empty file and an empty folder.
pub fn sample_tree(root: &Path) -> PathBuf {
    let top = root.join("photos");
    fill(&top.join("big.bin"), 9 * 1024 * 1024 + 123, 1);
    fill(&top.join("nested/deep/small.bin"), 4_321, 2);
    fill(&top.join("nested/other.bin"), 300_000, 3);
    std::fs::write(top.join("empty.txt"), b"").unwrap();
    std::fs::write(top.join("with space and ñ.txt"), "hola").unwrap();
    std::fs::create_dir_all(top.join("empty-folder")).unwrap();
    top
}

/// Every file under `root`, as (relative path, content), sorted.
pub fn snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if name.starts_with(".carrick-") {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else {
                let relative = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
                out.push((relative, std::fs::read(&path).unwrap()));
            }
        }
    }
    out.sort();
    out
}

/// The view of the send or receive the screen shows.
pub fn sending(app: &App) -> carrick_gui::job::JobView {
    app.send.current.as_ref().expect("a send on screen").lock().unwrap().clone()
}

pub fn receiving(app: &App) -> carrick_gui::job::JobView {
    app.receive.current.as_ref().expect("a receive on screen").lock().unwrap().clone()
}
