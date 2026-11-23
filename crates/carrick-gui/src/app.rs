//! The window: three screens over the core.
//!
//! - **Send**: choose or drop files or a folder, then show the code.
//! - **Receive**: enter the code, choose a folder, follow the progress.
//! - **Transfers**: what runs in this window, and what was left unfinished
//!   on this computer, to continue it.

use std::{
    path::{Path, PathBuf},
    sync::{mpsc, Arc, Mutex},
    time::{Duration, Instant},
};

use eframe::egui::{self, Align, Layout, RichText, ScrollArea, TextEdit, Ui, ViewportCommand};
use carrick_core::{
    events::Route,
    home::{Existing, Home, Record, Role, State},
    net::NetOptions,
    recv::{self, ReceiveOptions},
    send::{self, SendOptions},
};

use crate::{
    job::Pace,
    fmt,
    job::{Finish, Job, JobView, Kind, Phase},
    look,
};

/// What the window works with. The tests give their own.
pub struct Settings {
    /// Where transfers are recorded.
    pub home: Home,
    pub net: NetOptions,
    /// Show a system notification when a transfer ends.
    pub notify: bool,
    /// The folder the receive screen offers first.
    pub default_dest: Option<PathBuf>,
    /// Worker threads for transfers.
    pub threads: usize,
}

impl Settings {
    /// As the terminal reads them: `CARRICK_HOME`, `CARRICK_RELAY`,
    /// `CARRICK_NO_NOTIFY`, `CARRICK_THREADS`. Files go to Downloads.
    pub fn from_env() -> anyhow::Result<Self> {
        let dirs = directories::UserDirs::new();
        let default_dest = dirs
            .as_ref()
            .and_then(|d| d.download_dir().map(Path::to_path_buf))
            .or_else(|| dirs.as_ref().map(|d| d.home_dir().to_path_buf()));
        let threads = std::env::var("CARRICK_THREADS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(4));
        Ok(Self {
            home: Home::open()?,
            net: NetOptions::from_env()?,
            notify: std::env::var_os("CARRICK_NO_NOTIFY").is_none(),
            default_dest,
            threads: threads.max(2),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Send,
    Receive,
    Transfers,
    About,
}

/// The send screen: what is chosen, and the send it started.
pub struct SendForm {
    pub paths: Vec<PathBuf>,
    /// Give a ticket instead of a four-word code.
    pub ticket: bool,
    pub current: Option<Arc<Mutex<JobView>>>,
    copied: Option<Instant>,
}

/// The receive screen: what was entered, and the receive it started.
pub struct ReceiveForm {
    pub code: String,
    pub dest: String,
    /// What to do with files that already exist; asking by default.
    pub existing: Existing,
    pub current: Option<Arc<Mutex<JobView>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Picking {
    Files,
    Folder,
    Destination,
}

/// A file dialog that is open, on a thread of its own so the window keeps drawing.
struct Dialog {
    what: Picking,
    rx: mpsc::Receiver<Vec<PathBuf>>,
}

pub struct App {
    settings: Settings,
    runtime: Option<tokio::runtime::Runtime>,
    pub screen: Screen,
    pub send: SendForm,
    pub receive: ReceiveForm,
    /// Transfers started in this window, newest first.
    pub jobs: Vec<Job>,
    /// Unfinished transfers on this computer that this window is not running.
    pub unfinished: Vec<Record>,
    scanned: Option<Instant>,
    /// Something the window itself could not do.
    pub problem: Option<String>,
    dialog: Option<Dialog>,
    closing: Option<Instant>,
    /// The unfinished transfer whose Forget is waiting to be confirmed.
    forgetting: Option<String>,
    /// Light, dark or as the system is, as last saved; None until read.
    theme: Option<egui::ThemePreference>,
}

/// How long closing waits for transfers to stop cleanly.
const CLOSE_LIMIT: Duration = Duration::from_secs(10);

impl App {
    pub fn new(settings: Settings) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(settings.threads)
            .thread_name("carrick")
            .enable_all()
            .build()
            .expect("start the runtime");
        let dest = settings.default_dest.as_ref().map(|d| d.display().to_string()).unwrap_or_default();
        Self {
            settings,
            runtime: Some(runtime),
            screen: Screen::Send,
            send: SendForm { paths: Vec::new(), ticket: false, current: None, copied: None },
            receive: ReceiveForm { code: String::new(), dest, existing: Existing::Ask, current: None },
            jobs: Vec::new(),
            unfinished: Vec::new(),
            scanned: None,
            problem: None,
            dialog: None,
            closing: None,
            forgetting: None,
            theme: None,
        }
    }

    fn runtime(&self) -> &tokio::runtime::Handle {
        self.runtime.as_ref().expect("runtime").handle()
    }

    pub fn running(&self) -> usize {
        self.jobs.iter().filter(|j| j.is_running()).count()
    }

    /// Draws the whole window. eframe calls it every frame, and so do the tests.
    pub fn show(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        // The theme chosen last time, then saved whenever it changes.
        let theme = *self.theme.get_or_insert_with(|| {
            let saved = read_theme(&self.settings.home);
            ctx.set_theme(saved);
            saved
        });
        let now = ctx.options(|o| o.theme_preference);
        if now != theme {
            write_theme(&self.settings.home, now);
            self.theme = Some(now);
        }
        self.handle_close(&ctx);
        self.poll_dialog();
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if !dropped.is_empty() {
            self.choose(dropped);
            self.screen = Screen::Send;
        }
        if self.screen == Screen::Transfers && self.scanned.is_none_or(|at| at.elapsed() > Duration::from_secs(2)) {
            self.scan();
        }

        egui::Panel::top("tabs")
            .frame(egui::Frame::new().fill(ui.visuals().panel_fill).inner_margin(egui::Margin { left: 18, right: 18, top: 8, bottom: 0 }))
            .show(ui, |ui| self.tabs(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                column(ui, |ui| match self.screen {
                    Screen::Send => self.send_screen(ui, hovering),
                    Screen::Receive => self.receive_screen(ui),
                    Screen::Transfers => self.transfers_screen(ui),
                    Screen::About => self.about_screen(ui),
                });
            });
        });
        if hovering && self.screen != Screen::Send {
            look::drop_cover(&ctx);
        }
        if self.running() > 0 || self.closing.is_some() {
            // Speeds, times left and countdowns move even without events.
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    /// Closing stops running transfers first, so they are left resumable.
    fn handle_close(&mut self, ctx: &egui::Context) {
        let running = self.running() > 0;
        if ctx.input(|i| i.viewport().close_requested()) && running {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            if self.closing.is_none() {
                self.jobs.iter().filter(|j| j.is_running()).for_each(Job::stop);
                self.closing = Some(Instant::now());
            }
        }
        if let Some(since) = self.closing {
            if !running || since.elapsed() > CLOSE_LIMIT {
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
    }

    fn tabs(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Carrick").strong().size(18.0));
            ui.add_space(14.0);
            let running = self.running();
            for (screen, label) in [(Screen::Send, "Send"), (Screen::Receive, "Receive"), (Screen::Transfers, "Transfers")] {
                let text = if screen == Screen::Transfers && running > 0 { format!("{label} ({running})") } else { label.to_string() };
                if look::tab(ui, self.screen == screen, label, &text).clicked() {
                    self.screen = screen;
                    if screen == Screen::Transfers {
                        self.scanned = None;
                    }
                }
            }
            // On the right: light, dark or system, and About.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let mut theme = ui.ctx().options(|o| o.theme_preference);
                let before = theme;
                ui.spacing_mut().item_spacing.x = 2.0;
                ui.selectable_value(&mut theme, egui::ThemePreference::Dark, egui::ThemePreference::Dark.icon()).on_hover_text("Dark");
                ui.selectable_value(&mut theme, egui::ThemePreference::Light, egui::ThemePreference::Light.icon()).on_hover_text("Light");
                ui.selectable_value(&mut theme, egui::ThemePreference::System, egui::ThemePreference::System.icon()).on_hover_text("As the system is");
                if theme != before {
                    ui.ctx().set_theme(theme);
                }
                ui.add_space(10.0);
                if look::tab(ui, self.screen == Screen::About, "About", "About").clicked() {
                    self.screen = Screen::About;
                }
            });
        });
        ui.separator();
    }

    // ---------------------------------------------------------------- send

    /// Adds files or folders to send. Choosing while a send is on screen
    /// starts a new one; the old one carries on under Transfers.
    pub fn choose(&mut self, paths: Vec<PathBuf>) {
        if self.send.current.is_some() {
            self.send.current = None;
            self.send.paths.clear();
        }
        for path in paths {
            if !self.send.paths.contains(&path) {
                self.send.paths.push(path);
            }
        }
    }

    fn send_screen(&mut self, ui: &mut Ui, hovering: bool) {
        if let Some(current) = self.send.current.clone() {
            let view = current.lock().expect("view").clone();
            self.code_box(ui, &view);
            ui.add_space(10.0);
            if let Some(action) = job_card(ui, &view, false) {
                self.act(action, &current);
            }
            ui.add_space(10.0);
            if ui.button("Send something else").clicked() {
                self.send.current = None;
                self.send.paths.clear();
            }
            return;
        }

        ui.heading("Send files");
        ui.add_space(4.0);
        let mut pick = None;
        look::drop_zone(ui, hovering, |ui| {
            ui.label(RichText::new("Drop files or a folder here").size(18.0));
            ui.add_space(6.0);
            ui.label(RichText::new("or").color(look::weak(ui)));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                // Centre the two buttons in the zone.
                let width = 2.0 * 150.0 + ui.spacing().item_spacing.x;
                ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
                let busy = self.dialog.is_some();
                if ui.add_enabled(!busy, egui::Button::new("Choose files…").min_size(egui::vec2(150.0, 32.0))).clicked() {
                    pick = Some(Picking::Files);
                }
                if ui.add_enabled(!busy, egui::Button::new("Choose folder…").min_size(egui::vec2(150.0, 32.0))).clicked() {
                    pick = Some(Picking::Folder);
                }
            });
        });
        if let Some(what) = pick {
            self.pick(ui.ctx(), what);
        }

        if !self.send.paths.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("To send:").strong());
            let mut remove = None;
            for (i, path) in self.send.paths.iter().enumerate() {
                ui.horizontal(|ui| {
                    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.display().to_string());
                    ui.label(RichText::new(name).strong());
                    let parent = path.parent().map(|p| p.display().to_string()).unwrap_or_default();
                    ui.label(RichText::new(parent).small().color(look::weak(ui)));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("Remove").on_hover_text("Do not send this").clicked() {
                            remove = Some(i);
                        }
                    });
                });
            }
            if let Some(i) = remove {
                self.send.paths.remove(i);
            }
        }

        ui.add_space(8.0);
        ui.checkbox(&mut self.send.ticket, "Use a ticket instead of a code (works without Internet)");
        ui.add_space(8.0);
        if look::primary(ui, !self.send.paths.is_empty(), "Start sending").clicked() {
            self.start_send(ui.ctx());
        }
    }

    /// The code in large type, with a button to copy it.
    fn code_box(&mut self, ui: &mut Ui, view: &JobView) {
        let key = view.code.clone().map(|c| (c, true)).or_else(|| view.ticket.clone().map(|t| (t, false)));
        let Some((key, is_code)) = key else {
            if !view.phase.is_over() {
                ui.heading("Preparing the files");
                ui.label("The code appears when they are ready.");
            }
            return;
        };
        ui.heading(format!("Ready to send {}", view.name));
        ui.add_space(4.0);
        look::card(ui, |ui| {
            ui.label(if is_code {
                "On the other computer, open Carrick, choose Receive and enter this code:"
            } else {
                "On the other computer, open Carrick, choose Receive and paste this ticket:"
            });
            ui.add_space(6.0);
            if is_code {
                // As large as fits on one line: the longest codes have 39 letters.
                let size = (ui.available_width() / (key.chars().count() as f32 * 0.61)).clamp(14.0, 28.0);
                ui.add(egui::Label::new(RichText::new(&key).monospace().size(size).strong()).wrap());
            } else {
                ui.add(egui::Label::new(RichText::new(&key).monospace().size(12.0)).wrap());
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Copy").clicked() {
                    ui.ctx().copy_text(key.clone());
                    self.send.copied = Some(Instant::now());
                }
                if self.send.copied.is_some_and(|at| at.elapsed() < Duration::from_secs(2)) {
                    ui.label(RichText::new("Copied").color(look::good(ui)));
                    ui.ctx().request_repaint_after(Duration::from_millis(300));
                }
            });
            if is_code {
                ui.add_space(4.0);
                ui.label(RichText::new(format!("In a terminal: carrick receive {key}")).small().color(look::weak(ui)));
            }
        });
    }

    fn start_send(&mut self, ctx: &egui::Context) {
        let paths = self.send.paths.clone();
        let ticket = self.send.ticket;
        let home = self.settings.home.clone();
        let net = self.settings.net.clone();
        let absolute: Vec<PathBuf> = paths.iter().map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone())).collect();
        let job = Job::start(
            self.runtime(),
            ctx,
            JobView::new(Kind::Send),
            self.settings.notify,
            move |record, view| {
                view.id.as_deref() == Some(record.id.as_str())
                    || (record.role == Role::Send && record.root.is_none() && record.sources == absolute)
            },
            move |events, stop| async move {
                let opts = SendOptions { paths, ticket, receivers: None, net };
                send::send(&home, opts, events, stop).await.map(Finish::Sent)
            },
        );
        self.send.current = Some(job.view.clone());
        self.send.copied = None;
        self.jobs.insert(0, job);
    }

    // ------------------------------------------------------------- receive

    fn receive_screen(&mut self, ui: &mut Ui) {
        if let Some(current) = self.receive.current.clone() {
            let view = current.lock().expect("view").clone();
            if let Some(action) = job_card(ui, &view, false) {
                self.act(action, &current);
            }
            ui.add_space(10.0);
            if ui.button("Receive something else").clicked() {
                self.receive.current = None;
                self.receive.code.clear();
            }
            return;
        }

        ui.heading("Receive files");
        ui.add_space(4.0);
        let label = ui.label(RichText::new("Code").strong());
        ui.add(
            TextEdit::singleline(&mut self.receive.code)
                .hint_text("the four words the sender sees, or a ticket")
                .desired_width(f32::INFINITY),
        )
        .labelled_by(label.id);
        ui.add_space(8.0);
        let label = ui.label(RichText::new("Save to").strong());
        let mut pick = false;
        ui.horizontal(|ui| {
            let button_width = 110.0;
            let width = ui.available_width() - button_width - ui.spacing().item_spacing.x;
            ui.add(TextEdit::singleline(&mut self.receive.dest).desired_width(width)).labelled_by(label.id);
            pick = ui
                .add_enabled(self.dialog.is_none(), egui::Button::new("Choose…").min_size(egui::vec2(button_width, 30.0)))
                .clicked();
        });
        if pick {
            self.pick(ui.ctx(), Picking::Destination);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("If a file already exists:");
            egui::ComboBox::from_id_salt("existing")
                .selected_text(existing_label(self.receive.existing))
                .show_ui(ui, |ui| {
                    for choice in [Existing::Ask, Existing::Replace, Existing::Skip, Existing::KeepBoth] {
                        ui.selectable_value(&mut self.receive.existing, choice, existing_label(choice));
                    }
                });
        });
        ui.label(
            RichText::new("Files already here and identical never travel. Keep both saves the incoming one beside the old, as name (2).ext.")
                .small()
                .color(look::weak(ui)),
        );
        ui.add_space(8.0);
        let ready = !self.receive.code.trim().is_empty() && !self.receive.dest.trim().is_empty();
        if look::primary(ui, ready, "Start receiving").clicked() {
            self.start_receive(ui.ctx());
        }
    }

    fn start_receive(&mut self, ctx: &egui::Context) {
        let key = self.receive.code.trim().to_string();
        let dest = PathBuf::from(self.receive.dest.trim());
        let existing = self.receive.existing;
        let (ask, answers) = recv::answers();
        let home = self.settings.home.clone();
        let net = self.settings.net.clone();
        let view = JobView { dest: Some(dest.clone()), ..JobView::new(Kind::Receive) };
        let mine = key.clone();
        let job = Job::start(
            self.runtime(),
            ctx,
            view,
            self.settings.notify,
            move |record, _| record.role == Role::Receive && record.key.as_deref() == Some(mine.as_str()),
            move |events, stop| async move {
                let opts = ReceiveOptions { key, dest, existing, answers: Some(answers), seed: false, net };
                recv::receive(&home, opts, events, stop).await.map(Finish::Received)
            },
        );
        let mut job = job;
        job.answer = Some(ask);
        self.receive.current = Some(job.view.clone());
        self.jobs.insert(0, job);
    }


    // --------------------------------------------------------------- about

    fn about_screen(&mut self, ui: &mut Ui) {
        ui.heading("About Carrick");
        ui.add_space(4.0);
        ui.label(RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION"))).strong());
        ui.label("Send files and folders from one computer to another: directly when the networks allow it, encrypted end to end, verified piece by piece, and carrying on by itself after a dropped connection.");
        ui.add_space(10.0);
        ui.label(RichText::new("Licence").strong());
        ui.label(format!("Copyright © 2026 {AUTHOR}. Carrick is free software, licensed under the MIT licence or the Apache License 2.0, at your option."));
        ui.hyperlink_to(REPOSITORY.trim_start_matches("https://"), REPOSITORY);
        ui.add_space(4.0);
        for (title, text) in [("MIT licence", LICENSE_MIT), ("Apache License 2.0", LICENSE_APACHE)] {
            ui.collapsing(title, |ui| {
                ScrollArea::vertical().id_salt(title).max_height(260.0).show(ui, |ui| {
                    ui.label(RichText::new(text).small().monospace());
                });
            });
        }
        ui.add_space(10.0);
        ui.label(RichText::new("Third-party software").strong());
        ui.label("Carrick is built on open-source libraries, among them iroh and iroh-blobs by n0, and egui. Their licences come with Carrick, in THIRD-PARTY-LICENSES.txt.");
        match third_party_licences() {
            Some(path) => {
                if ui.button("Open the third-party licences").clicked() {
                    if let Err(e) = open_in_system(&path) {
                        self.problem = Some(format!("Could not open {}: {e}", path.display()));
                    }
                }
                ui.label(RichText::new(path.display().to_string()).small().color(look::weak(ui)));
            }
            None => {
                ui.label(RichText::new("Not found next to this copy of Carrick (it comes with the installers).").small().color(look::weak(ui)));
            }
        }
        if let Some(problem) = &self.problem {
            ui.colored_label(look::bad(ui), problem);
        }
        ui.add_space(10.0);
        ui.label(RichText::new("Appearance").strong());
        ui.label("Light, dark, or as the system is: the buttons at the top right. Carrick remembers the choice.");
    }

    // ----------------------------------------------------------- transfers

    /// Reads the unfinished transfers of this computer from disk.
    pub fn scan(&mut self) {
        match self.settings.home.list() {
            Ok(records) => {
                self.unfinished = records
                    .into_iter()
                    .filter(|r| r.state == State::Active)
                    .filter(|r| !self.jobs.iter().any(|j| j.is_running() && j.owns(r)))
                    .rev()
                    .collect();
                self.problem = None;
            }
            Err(e) => self.problem = Some(format!("Could not read the list of transfers: {e:#}")),
        }
        self.scanned = Some(Instant::now());
    }

    fn transfers_screen(&mut self, ui: &mut Ui) {
        if let Some(problem) = &self.problem {
            ui.colored_label(look::bad(ui), problem);
        }
        if self.jobs.is_empty() && self.unfinished.is_empty() {
            ui.heading("Transfers");
            ui.add_space(4.0);
            ui.label("Nothing here yet. What you send or receive shows up here, and so does anything left unfinished on this computer, to continue it.");
            return;
        }
        let mut action = None;
        if !self.jobs.is_empty() {
            ui.heading("In this window");
            ui.add_space(4.0);
            for job in &self.jobs {
                let view = job.view.lock().expect("view").clone();
                if let Some(pressed) = job_card(ui, &view, true) {
                    action = Some((pressed, job.view.clone()));
                }
                ui.add_space(6.0);
            }
        }
        if let Some((action, view)) = action {
            self.act(action, &view);
        }

        let mut resume = None;
        let mut forget = None;
        if !self.unfinished.is_empty() {
            ui.add_space(8.0);
            ui.heading("Unfinished");
            ui.label(RichText::new("Stopped before the end on this computer. Continue picks up where it was.").color(look::weak(ui)));
            ui.add_space(4.0);
            for record in &self.unfinished {
                let asking = self.forgetting.as_deref() == Some(record.id.as_str());
                look::card(ui, |ui| {
                    // Buttons first, from the right, so long names are cut
                    // short with "…" instead of pushing the card wider.
                    ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if asking {
                            if ui.button("Keep").clicked() {
                                self.forgetting = None;
                            }
                            if ui.button(RichText::new("Forget").color(look::bad(ui))).clicked() {
                                forget = Some(record.id.clone());
                            }
                        } else {
                            if ui.button("Forget").on_hover_text("Take it off this list").clicked() {
                                self.forgetting = Some(record.id.clone());
                            }
                            if ui.button("Continue").clicked() {
                                resume = Some(record.clone());
                            }
                        }
                        ui.with_layout(Layout::top_down(Align::Min), |ui| {
                            let what = match record.role {
                                Role::Send => "Sending",
                                Role::Receive => "Receiving",
                            };
                            let name = if record.name.is_empty() { "(not listed yet)" } else { &record.name };
                            ui.add(egui::Label::new(RichText::new(format!("{what} {name}")).strong()).truncate());
                            let mut line = Vec::new();
                            if record.total_bytes > 0 {
                                line.push(format!("{}, {}", fmt::files(record.files), fmt::bytes(record.total_bytes)));
                            }
                            match record.role {
                                Role::Send => line.push(match &record.code {
                                    Some(code) => format!("code {code}"),
                                    None => "with a ticket".to_string(),
                                }),
                                Role::Receive => {
                                    if let Some(dest) = &record.dest {
                                        line.push(format!("into {}", dest.display()));
                                    }
                                }
                            }
                            ui.add(egui::Label::new(RichText::new(line.join(" · ")).small().color(look::weak(ui))).truncate());
                        });
                    });
                    });
                    if asking {
                        let text = match record.role {
                            Role::Receive => "Forget it? Pieces that had not become whole files are deleted; the files already in the destination stay.",
                            Role::Send => "Forget it? It can no longer be continued; your files are not touched.",
                        };
                        ui.label(RichText::new(text).color(look::caution(ui)));
                    }
                });
                ui.add_space(6.0);
            }
        }
        if let Some(record) = resume {
            self.forgetting = None;
            self.resume(ui.ctx(), record);
        }
        if let Some(id) = forget {
            self.forgetting = None;
            if let Err(e) = self.settings.home.forget(&id) {
                self.problem = Some(format!("Could not forget that transfer: {e:#}"));
            }
            self.scan();
        }
    }

    /// Continues an unfinished transfer, as `carrick resume` does.
    pub fn resume(&mut self, ctx: &egui::Context, record: Record) {
        let id = record.id.clone();
        let role = record.role;
        let home = self.settings.home.clone();
        let (ask, answers) = recv::answers();
        let mut job = Job::start(
            self.runtime(),
            ctx,
            JobView::from_record(&record),
            self.settings.notify,
            move |r, _| r.id == id,
            move |events, stop| async move {
                match role {
                    Role::Send => send::resume_send(&home, record, events, stop).await.map(Finish::Sent),
                    Role::Receive => recv::resume_receive(&home, record, Some(answers), events, stop).await.map(Finish::Received),
                }
            },
        );
        job.answer = Some(ask);
        self.jobs.insert(0, job);
        self.scan();
    }

    /// Does what a button of a transfer card asks.
    fn act(&mut self, action: CardAction, view: &Arc<Mutex<JobView>>) {
        match action {
            CardAction::Stop => self.stop(view),
            CardAction::Remove => self.jobs.retain(|j| !Arc::ptr_eq(&j.view, view)),
            CardAction::Answer(choice) => {
                if let Some(job) = self.jobs.iter().find(|j| Arc::ptr_eq(&j.view, view)) {
                    job.answer(choice);
                }
            }
        }
    }

    fn stop(&mut self, view: &Arc<Mutex<JobView>>) {
        if let Some(job) = self.jobs.iter().find(|j| Arc::ptr_eq(&j.view, view)) {
            job.stop();
        }
        self.scanned = None;
    }

    // ------------------------------------------------------------- dialogs

    fn pick(&mut self, ctx: &egui::Context, what: Picking) {
        if self.dialog.is_some() {
            return;
        }
        let start = match what {
            Picking::Destination => Some(PathBuf::from(self.receive.dest.trim())).filter(|p| p.is_dir()),
            _ => None,
        };
        let (tx, rx) = mpsc::channel();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let mut dialog = rfd::FileDialog::new();
            if let Some(dir) = start {
                dialog = dialog.set_directory(dir);
            }
            let picked = match what {
                Picking::Files => dialog.set_title("Choose files to send").pick_files(),
                Picking::Folder => dialog.set_title("Choose a folder to send").pick_folder().map(|p| vec![p]),
                Picking::Destination => dialog.set_title("Choose where to save").pick_folder().map(|p| vec![p]),
            };
            let _ = tx.send(picked.unwrap_or_default());
            repaint.request_repaint();
        });
        self.dialog = Some(Dialog { what, rx });
    }

    fn poll_dialog(&mut self) {
        let Some(dialog) = &self.dialog else { return };
        let picked = match dialog.rx.try_recv() {
            Ok(paths) => paths,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Vec::new(),
        };
        let what = dialog.what;
        self.dialog = None;
        if picked.is_empty() {
            return;
        }
        match what {
            Picking::Files | Picking::Folder => self.choose(picked),
            Picking::Destination => self.receive.dest = picked[0].display().to_string(),
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        // Stopped transfers save their state; give them a moment to.
        self.jobs.iter().filter(|j| j.is_running()).for_each(Job::stop);
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(Duration::from_secs(3));
        }
    }
}

/// Who holds the copyright, and where the source is.
const AUTHOR: &str = "Johan Socarras";
const REPOSITORY: &str = "https://github.com/johan-socarras/Carrick";
const LICENSE_MIT: &str = include_str!("../../../LICENSE-MIT");
const LICENSE_APACHE: &str = include_str!("../../../LICENSE-APACHE");

/// The licences of the libraries in Carrick, where the installers put them:
/// next to the program on Windows, in the package's documentation on Linux.
fn third_party_licences() -> Option<PathBuf> {
    let mut places = Vec::new();
    if let Some(dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(PathBuf::from)) {
        places.push(dir.join("THIRD-PARTY-LICENSES.txt"));
    }
    places.push(PathBuf::from("/usr/share/doc/carrick/THIRD-PARTY-LICENSES.txt"));
    places.into_iter().find(|p| p.is_file())
}

/// Opens a file with the program the system uses for it.
fn open_in_system(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    let mut command = {
        let mut c = std::process::Command::new("explorer");
        c.arg(path);
        c
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(path);
        c
    };
    command.spawn().map(|_| ())
}

/// Where the window remembers light, dark or system.
fn theme_file(home: &carrick_core::home::Home) -> PathBuf {
    home.root().join("carrick-gui.theme")
}

fn read_theme(home: &carrick_core::home::Home) -> egui::ThemePreference {
    match std::fs::read_to_string(theme_file(home)).as_deref().map(str::trim) {
        Ok("light") => egui::ThemePreference::Light,
        Ok("dark") => egui::ThemePreference::Dark,
        _ => egui::ThemePreference::System,
    }
}

fn write_theme(home: &carrick_core::home::Home, theme: egui::ThemePreference) {
    let word = match theme {
        egui::ThemePreference::Light => "light",
        egui::ThemePreference::Dark => "dark",
        egui::ThemePreference::System => "system",
    };
    let _ = std::fs::write(theme_file(home), word);
}

/// The centred column every screen sits in.
fn column(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let width = ui.available_width();
    let inner = width.min(600.0);
    let side = ((width - inner) / 2.0).max(0.0);
    ui.add_space(14.0);
    ui.horizontal_top(|ui| {
        ui.add_space(side + 18.0);
        ui.vertical(|ui| {
            ui.set_width(inner - 36.0);
            add(ui);
        });
    });
    ui.add_space(20.0);
}

/// What a button on a transfer card asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardAction {
    Stop,
    /// Take a finished transfer off the list of this window.
    Remove,
    /// The answer about files that already exist.
    Answer(Existing),
}

/// How the choice about files that already exist reads.
fn existing_label(choice: Existing) -> &'static str {
    match choice {
        Existing::Ask => "Ask me",
        Existing::Replace => "Replace them",
        Existing::Skip => "Skip them",
        Existing::KeepBoth => "Keep both",
    }
}

/// One transfer: where it stands, its bar, its route and its receivers.
/// `removable` puts "Remove from list" on a finished one.
fn job_card(ui: &mut Ui, view: &JobView, removable: bool) -> Option<CardAction> {
    look::card(ui, |ui| {
        // The size first, from the right; the title takes what is left and
        // is cut short with "…" rather than push the card wider.
        // Inside a row, so the layout takes one line, not the whole card.
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if view.total > 0 || view.files > 0 {
                    ui.label(RichText::new(format!("{}, {}", fmt::files(view.files), fmt::bytes(view.total))).color(look::weak(ui)));
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(RichText::new(view.title()).strong().size(16.0)).truncate());
                });
            });
        });
        if let Some((files, bytes, examples)) = &view.asking {
            let mut answer = None;
            look::card(ui, |ui| {
                ui.label(
                    RichText::new(format!(
                        "{} already {} here with different content ({}), like {}.",
                        fmt::files(*files),
                        if *files == 1 { "exists" } else { "exist" },
                        fmt::bytes(*bytes),
                        examples.join(", ")
                    ))
                    .strong(),
                );
                ui.label(RichText::new("The copy waits until you choose. Files that are identical never travel.").color(look::weak(ui)));
                ui.horizontal(|ui| {
                    for choice in [Existing::Replace, Existing::Skip, Existing::KeepBoth] {
                        if ui.button(existing_label(choice)).clicked() {
                            answer = Some(choice);
                        }
                    }
                });
            });
            if let Some(choice) = answer {
                return Some(CardAction::Answer(choice));
            }
        }
        status(ui, view);
        if let Some(fraction) = view.fraction() {
            ui.add(egui::ProgressBar::new(fraction).desired_height(14.0).show_percentage());
        }
        if !view.phase.is_over() {
            if let Some(route) = view.route {
                route_line(ui, route);
            }
        }
        if view.kind == Kind::Send && !view.receivers.is_empty() {
            ui.add_space(4.0);
            for r in &view.receivers {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Receiver {}", r.id)).monospace());
                    match r.route {
                        Some(Route::Direct) => ui.label(RichText::new("direct").color(look::good(ui))),
                        Some(Route::Relay) => ui.label(RichText::new("through a relay").color(look::caution(ui))),
                        None => ui.label(""),
                    };
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if r.done {
                            ui.label(RichText::new("has everything").color(look::good(ui)));
                        } else if !r.connected {
                            ui.label(RichText::new("disconnected").color(look::weak(ui)));
                        } else {
                            ui.label(RichText::new(format!("{} of {}", fmt::bytes(r.have), fmt::bytes(r.total))).color(look::weak(ui)));
                        }
                    });
                });
                let pace = view.receiver_pace.get(&r.id);
                let now = Instant::now();
                if r.connected && !r.done {
                    ui.add(egui::ProgressBar::new(fmt::fraction(r.have, r.total)).desired_height(10.0));
                    if let Some(left) = pace.and_then(|p| p.left(r.have, r.total, now)) {
                        ui.label(RichText::new(format!("{} left", fmt::eta(left))).color(look::weak(ui)));
                    }
                }
                if let Some(pace) = pace {
                    pace_line(ui, pace, now);
                }
            }
        }
        if let Some((files, bytes)) = view.already.filter(|(files, _)| *files > 0) {
            ui.label(
                RichText::new(format!("{}, {}, already here and identical: they did not travel.", fmt::files(files), fmt::bytes(bytes)))
                    .color(look::weak(ui)),
            );
        }
        if !view.kept_aside.is_empty() {
            ui.label(format!(
                "{} already existed with different content; the incoming ones were saved beside them, like {}.",
                fmt::files(view.kept_aside.len() as u64),
                view.kept_aside[0].file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
            ));
        }
        if let Some(note) = &view.note {
            if !view.phase.is_over() {
                ui.label(RichText::new(note).color(look::weak(ui)));
            }
        }
        ui.add_space(2.0);
        if !view.phase.is_over() {
            ui.add_enabled(!view.stopping, egui::Button::new("Stop")).clicked().then_some(CardAction::Stop)
        } else if removable {
            ui.button("Remove from list").clicked().then_some(CardAction::Remove)
        } else {
            None
        }
    })
}

fn status(ui: &mut Ui, view: &JobView) {
    if view.stopping && !view.phase.is_over() {
        ui.label("Stopping…");
        return;
    }
    if view.asking.is_some() {
        ui.label(RichText::new("Waiting for your answer…").color(look::caution(ui)));
        return;
    }
    match &view.phase {
        Phase::Starting => {
            ui.label("Starting…");
        }
        Phase::Preparing { done, total } => {
            ui.label(format!("Preparing: {} of {}", fmt::bytes(*done), fmt::bytes(*total)));
        }
        Phase::Offering => {
            let waiting = view.receivers.iter().all(|r| !r.connected || r.done);
            if view.receivers.is_empty() {
                let key = if view.code.is_some() { "the code" } else { "the ticket" };
                ui.label(format!("Waiting for someone to enter {key}. Keep Carrick open until they have everything."));
            } else if waiting {
                let done = view.receivers.iter().filter(|r| r.done).count();
                let who = match (done, view.receivers.len()) {
                    (1, 1) => "The receiver has everything.".to_string(),
                    (done, all) => format!("{done} of {all} receivers have everything."),
                };
                ui.label(format!("{who} Still on offer until you stop it."));
            } else {
                let busy = view.receivers.iter().filter(|r| r.connected && !r.done).count();
                ui.label(if busy == 1 { "Sending to 1 receiver".to_string() } else { format!("Sending to {busy} receivers") });
            }
        }
        Phase::Looking { attempt } => {
            if *attempt <= 1 {
                ui.label("Looking for the sender…");
            } else {
                ui.label(format!("Looking for the sender again (attempt {attempt})…"));
            }
        }
        Phase::Checking { .. } => {
            ui.label("Checking what is already here");
        }
        Phase::Receiving => {
            let now = Instant::now();
            let mut line = format!("{} of {}", fmt::bytes(view.have), fmt::bytes(view.total));
            if let Some(left) = view.pace.left(view.have, view.total, now) {
                line.push_str(&format!("  ·  {} left", fmt::eta(left)));
            }
            ui.label(line);
            pace_line(ui, &view.pace, now);
        }
        Phase::Waiting { reason, seconds } => {
            ui.label(RichText::new(format!("Interrupted: {reason}. Trying again in {seconds} s…")).color(look::caution(ui)));
        }
        Phase::Saving { done, total } => {
            ui.label(format!("Saving files: {done} of {total}"));
        }
        Phase::Done => match view.kind {
            Kind::Send => {
                let done = view.receivers.iter().filter(|r| r.done).count();
                let who = if done == 1 { "1 receiver has".to_string() } else { format!("{done} receivers have") };
                look::dot_label(ui, look::good(ui), format!("Done: {who} everything."));
            }
            Kind::Receive => {
                let place = view.dest.as_ref().map(|d| d.display().to_string()).unwrap_or_default();
                look::dot_label(ui, look::good(ui), format!("Done: saved in {place}"));
                pace_line(ui, &view.pace, Instant::now());
            }
        },
        Phase::Stopped => {
            let text = match view.kind {
                Kind::Send => "Stopped. It can be offered again from Unfinished, under Transfers.",
                Kind::Receive => "Stopped. What arrived is kept: continue it from Unfinished, under Transfers.",
            };
            ui.label(RichText::new(text).color(look::caution(ui)));
        }
        Phase::Failed(error) => {
            ui.label(RichText::new(format!("Failed: {error}")).color(look::bad(ui)));
        }
    }
}

/// The speed now, the average since the copy began, and the time it has
/// taken: while it runs, and frozen once it ends.
fn pace_line(ui: &mut Ui, pace: &Pace, at: Instant) {
    let mut parts = Vec::new();
    if let Some(speed) = pace.now() {
        parts.push(format!("now {}/s", fmt::bytes(speed as u64)));
    }
    if let Some(speed) = pace.average(at) {
        parts.push(format!("average {}/s", fmt::bytes(speed as u64)));
    }
    if let Some(taken) = pace.taken(at) {
        parts.push(format!("{} so far", fmt::eta(taken)));
    }
    if parts.is_empty() {
        return;
    }
    let mut text = parts.join("  ·  ");
    if let Some(first) = text.get(0..1) {
        text = first.to_uppercase() + &text[1..];
    }
    ui.add(egui::Label::new(RichText::new(text).color(look::weak(ui))).truncate());
}

fn route_line(ui: &mut Ui, route: Route) {
    match route {
        Route::Direct => look::dot_label(ui, look::good(ui), "Direct, computer to computer"),
        Route::Relay => look::dot_label(
            ui,
            look::caution(ui),
            "Through a relay, still encrypted end to end. The free public relays are slow on purpose.",
        ),
    }
}
