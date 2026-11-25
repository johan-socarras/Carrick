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
    home::{now_secs, Existing, Home, Record, Role, State},
    net::NetOptions,
    recv::{self, ReceiveOptions},
    running::{Finished, Progress, Stage},
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
    /// When the command for a terminal was copied.
    copied_command: Option<Instant>,
}

/// The receive screen: what was entered, and the receive it started.
pub struct ReceiveForm {
    pub code: String,
    pub dest: String,
    /// What to do with files that already exist; asking by default.
    pub existing: Existing,
    pub current: Option<Arc<Mutex<JobView>>>,
    /// Folders received into lately, newest first, kept between runs.
    pub recent: Vec<PathBuf>,
}

/// How many folders "Save to" remembers.
pub const RECENT_FOLDERS: usize = 5;

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
    /// Unfinished transfers on this computer that nothing is running.
    pub unfinished: Vec<Record>,
    /// Transfers another Carrick on this computer is running (a terminal,
    /// another window), with where they stand when they have said.
    pub elsewhere: Vec<(Record, Option<Progress>)>,
    /// The last transfers that ended well, newest first, and whether they
    /// are on screen.
    pub history: Vec<Finished>,
    pub show_history: bool,
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
        // The folder used last time, or the default one the first time.
        let recent = read_folders(&settings.home);
        let dest = recent
            .first()
            .or(settings.default_dest.as_ref())
            .map(|d| d.display().to_string())
            .unwrap_or_default();
        Self {
            settings,
            runtime: Some(runtime),
            screen: Screen::Send,
            send: SendForm { paths: Vec::new(), ticket: false, current: None, copied: None, copied_command: None },
            receive: ReceiveForm { code: String::new(), dest, existing: Existing::Ask, current: None, recent },
            jobs: Vec::new(),
            unfinished: Vec::new(),
            elsewhere: Vec::new(),
            history: Vec::new(),
            show_history: false,
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
        self.line_up();
        self.poll_dialog();
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if !dropped.is_empty() {
            self.choose(dropped);
            self.screen = Screen::Send;
        }
        if self.screen == Screen::Transfers {
            // What runs elsewhere moves on its own; look again more often.
            let every = if self.elsewhere.is_empty() { Duration::from_secs(2) } else { Duration::from_secs(1) };
            if self.scanned.is_none_or(|at| at.elapsed() > every) {
                self.scan();
            }
            if !self.elsewhere.is_empty() {
                ctx.request_repaint_after(every);
            }
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
            // No name here: the title bar of the window already says it. The
            // knot of the icon instead.
            look::knot(ui);
            ui.add_space(6.0);
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
        ui.checkbox(&mut self.send.ticket, "No Internet? Use a long code instead of four words");
        ui.label(
            RichText::new("Works when both computers are on the same network. The long code has to be copied and pasted; it cannot be read out.")
                .small()
                .color(look::weak(ui)),
        );
        ui.add_space(8.0);
        if look::primary(ui, !self.send.paths.is_empty(), "Start sending").clicked() {
            self.start_send(ui.ctx());
        }
    }

    /// The code in large type, with a button to copy it.
    fn code_box(&mut self, ui: &mut Ui, view: &JobView) {
        let key = view.code.clone().map(|c| (c, true)).or_else(|| view.ticket.clone().map(|t| (t, false)));
        let Some((key, is_code)) = key else {
            if view.phase == Phase::Queued {
                ui.heading(format!("{} is in line", view.name));
                ui.label("It goes on offer when the send before it is done. The long code appears then: it carries addresses only known at that moment.");
            } else if !view.phase.is_over() {
                ui.heading("Preparing the files");
                ui.label("The code appears when they are ready.");
            }
            return;
        };
        if view.phase == Phase::Queued {
            ui.heading(format!("{} is in line", view.name));
            ui.label(RichText::new("It goes on offer when the send before it is done. The code already works: the other computer waits until then and starts by itself.").color(look::weak(ui)));
        } else {
            ui.heading(format!("Ready to send {}", view.name));
        }
        ui.add_space(4.0);
        look::card(ui, |ui| {
            ui.label(if is_code {
                "On the other computer, open Carrick, choose Receive and enter this code:"
            } else {
                "On the other computer, open Carrick, choose Receive and paste this long code:"
            });
            ui.add_space(6.0);
            let color = look::code_color(ui);
            look::code_frame(ui, |ui| {
                if is_code {
                    // As large as fits on one line: the longest codes have 39 letters.
                    let size = (ui.available_width() / (key.chars().count() as f32 * 0.61)).clamp(14.0, 28.0);
                    ui.add(egui::Label::new(RichText::new(&key).monospace().size(size).strong().color(color)).wrap());
                } else {
                    ui.add(egui::Label::new(RichText::new(&key).monospace().size(12.0).color(color)).wrap());
                }
            });
            ui.add_space(6.0);
            // The code, or the whole command for a terminal (PowerShell or any other).
            let command = format!("carrick receive {key}");
            ui.horizontal(|ui| {
                if look::brand(ui, "Copy").clicked() {
                    ui.ctx().copy_text(key.clone());
                    self.send.copied = Some(Instant::now());
                    self.send.copied_command = None;
                }
                if ui.button("Copy command").on_hover_text("Copies the command to receive this in a terminal").clicked() {
                    ui.ctx().copy_text(command.clone());
                    self.send.copied_command = Some(Instant::now());
                    self.send.copied = None;
                }
                let recent = |at: Option<Instant>| at.is_some_and(|at| at.elapsed() < Duration::from_secs(2));
                if recent(self.send.copied) {
                    ui.label(RichText::new("Copied").color(look::good(ui)));
                    ui.ctx().request_repaint_after(Duration::from_millis(300));
                } else if recent(self.send.copied_command) {
                    ui.label(RichText::new("Command copied").color(look::good(ui)));
                    ui.ctx().request_repaint_after(Duration::from_millis(300));
                }
            });
            ui.add_space(4.0);
            let line = if is_code { format!("In a terminal: {command}") } else { "In a terminal: carrick receive and the long code".to_string() };
            ui.add(egui::Label::new(RichText::new(line).small().color(look::weak(ui))).truncate());
        });
    }

    fn start_send(&mut self, ctx: &egui::Context) {
        let paths = self.send.paths.clone();
        let ticket = self.send.ticket;
        let home = self.settings.home.clone();
        let net = self.settings.net.clone();
        let absolute: Vec<PathBuf> = paths.iter().map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone())).collect();
        let (go, turn) = tokio::sync::watch::channel(self.line_is_free());
        let mut job = Job::start(
            self.runtime(),
            ctx,
            JobView::new(Kind::Send),
            self.settings.notify,
            move |record, view| {
                view.id.as_deref() == Some(record.id.as_str())
                    || (record.role == Role::Send && record.root.is_none() && record.sources == absolute)
            },
            move |events, stop| async move {
                let opts = SendOptions { paths, ticket, receivers: None, net, turn: Some(turn) };
                send::send(&home, opts, events, stop).await.map(Finish::Sent)
            },
        );
        job.turn = Some(go);
        job.place = self.next_place();
        self.send.current = Some(job.view.clone());
        self.send.copied = None;
        self.send.copied_command = None;
        self.jobs.insert(0, job);
    }

    // ------------------------------------------------------- line of sends

    /// The place after every send in line.
    fn next_place(&self) -> i64 {
        self.jobs.iter().filter(|j| j.turn.is_some()).map(|j| j.place + 1).max().unwrap_or(0)
    }

    /// Whether a send started now could go on at once: no send in line
    /// still has someone waiting for it.
    fn line_is_free(&self) -> bool {
        !self.jobs.iter().any(|j| j.turn.is_some() && j.is_running() && !served(&j.view.lock().expect("view")))
    }

    /// Gives the turn to the first send in line that still has work, and
    /// holds back the others. A send someone already has in full stays on
    /// offer for latecomers without holding the line.
    fn line_up(&self) {
        let mut sends: Vec<&Job> = self.jobs.iter().filter(|j| j.turn.is_some() && j.is_running()).collect();
        sends.sort_by_key(|j| j.place);
        let mut taken = false;
        for job in sends {
            let go = if served(&job.view.lock().expect("view")) {
                true
            } else if !taken {
                taken = true;
                true
            } else {
                false
            };
            if let Some(turn) = &job.turn {
                turn.send_if_modified(|now| std::mem::replace(now, go) != go);
            }
        }
    }

    /// Puts a send at the front of the line; the one it passes pauses and
    /// carries on after it.
    fn first(&mut self, view: &Arc<Mutex<JobView>>) {
        let front = self.jobs.iter().filter(|j| j.turn.is_some()).map(|j| j.place).min().unwrap_or(0);
        if let Some(job) = self.jobs.iter_mut().find(|j| Arc::ptr_eq(&j.view, view)) {
            job.place = front - 1;
        }
        self.line_up();
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
                .hint_text("the four words the sender sees, or the long code")
                .desired_width(f32::INFINITY),
        )
        .labelled_by(label.id);
        ui.add_space(8.0);
        let label = ui.label(RichText::new("Save to").strong());
        let mut pick = false;
        let mut chosen = None;
        let mut drop_folder = None;
        ui.horizontal(|ui| {
            let button_width = 110.0;
            let menu_width = 34.0;
            let width = ui.available_width() - button_width - menu_width - 2.0 * ui.spacing().item_spacing.x;
            ui.add(TextEdit::singleline(&mut self.receive.dest).desired_width(width)).labelled_by(label.id);
            // The folders used lately, to pick one without looking for it.
            let recent = &self.receive.recent;
            let current = self.receive.dest.trim().to_string();
            ui.add_enabled_ui(!recent.is_empty(), |ui| {
                ui.menu_button(RECENT_ICON, |ui| {
                    // A fixed width, so the crosses stay inside the window.
                    ui.set_width(400.0);
                    ui.label(RichText::new("Recent folders").small().color(look::weak(ui)));
                    for (i, folder) in recent.iter().enumerate() {
                        let shown = folder.display().to_string();
                        // The cross first, from the right, so a long path is
                        // cut short instead of pushing it out of sight.
                        ui.horizontal(|ui| {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.small_button(REMOVE_ICON).on_hover_text("Take it off this list").clicked() {
                                    drop_folder = Some(i);
                                }
                                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                    let item = egui::Button::selectable(same_folder(&shown, &current), &shown).truncate();
                                    if ui.add(item).clicked() {
                                        chosen = Some(shown.clone());
                                        ui.close();
                                    }
                                });
                            });
                        });
                    }
                })
                .response
                .on_hover_text("Folders used lately");
            });
            pick = ui
                .add_enabled(self.dialog.is_none(), egui::Button::new("Choose…").min_size(egui::vec2(button_width, 30.0)))
                .clicked();
        });
        if let Some(folder) = chosen {
            self.receive.dest = folder;
        }
        if let Some(i) = drop_folder {
            self.receive.recent.remove(i);
            write_folders(&self.settings.home, &self.receive.recent);
        }
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
        // Remembered first in the list, and offered first next time.
        let shown = dest.display().to_string();
        self.receive.recent.retain(|f| !same_folder(&f.display().to_string(), &shown));
        self.receive.recent.insert(0, dest.clone());
        self.receive.recent.truncate(RECENT_FOLDERS);
        write_folders(&self.settings.home, &self.receive.recent);
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
        ui.add_space(4.0);
        ui.label(WORD_LIST_CREDIT);
        ui.horizontal_wrapped(|ui| {
            ui.hyperlink_to("eff.org/dice", "https://www.eff.org/dice");
            ui.label("·");
            ui.hyperlink_to("CC BY 3.0 US", "https://creativecommons.org/licenses/by/3.0/us/");
        });
        ui.add_space(4.0);
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

    /// Reads from disk the transfers of this computer that did not finish:
    /// those another Carrick is running, and those nothing is running.
    pub fn scan(&mut self) {
        match self.settings.home.list() {
            Ok(records) => {
                let home = &self.settings.home;
                let (elsewhere, unfinished): (Vec<Record>, Vec<Record>) = records
                    .into_iter()
                    .filter(|r| r.state == State::Active)
                    .filter(|r| !self.jobs.iter().any(|j| j.is_running() && j.owns(r)))
                    .rev()
                    .partition(|r| home.is_running(&r.id));
                self.elsewhere = elsewhere
                    .into_iter()
                    .map(|r| {
                        let progress = home.progress(&r.id);
                        (r, progress)
                    })
                    .collect();
                self.unfinished = unfinished;
                self.problem = None;
            }
            Err(e) => self.problem = Some(format!("Could not read the list of transfers: {e:#}")),
        }
        self.history = self.settings.home.history();
        self.scanned = Some(Instant::now());
    }

    fn transfers_screen(&mut self, ui: &mut Ui) {
        if let Some(problem) = &self.problem {
            ui.colored_label(look::bad(ui), problem);
        }
        // Only what is being copied now: what ended goes to the history.
        let running: Vec<&Job> = self.jobs.iter().filter(|j| j.is_running()).collect();
        let mut action = None;
        if running.is_empty() && self.elsewhere.is_empty() {
            ui.heading("Transfers");
            ui.add_space(4.0);
            ui.label("Nothing is being copied right now.");
        } else {
            ui.heading("In progress");
            ui.add_space(4.0);
            for job in running {
                let view = job.view.lock().expect("view").clone();
                if let Some(pressed) = job_card(ui, &view, false) {
                    action = Some((pressed, job.view.clone()));
                }
                ui.add_space(6.0);
            }
            for (record, progress) in &self.elsewhere {
                elsewhere_card(ui, record, progress.as_ref());
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
                                    None => "with a long code".to_string(),
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

        ui.add_space(10.0);
        let label = if self.show_history { "Hide history" } else { "Show history" };
        if ui.button(label).on_hover_text("The last three transfers that ended well on this computer").clicked() {
            self.show_history = !self.show_history;
            self.history = self.settings.home.history();
        }
        if self.show_history {
            ui.add_space(4.0);
            if self.history.is_empty() {
                ui.label(RichText::new("No transfer has ended on this computer yet.").color(look::weak(ui)));
            }
            let now = now_secs();
            for finished in &self.history {
                history_card(ui, finished, now);
                ui.add_space(6.0);
            }
        }
    }

    /// Continues an unfinished transfer, as `carrick resume` does.
    pub fn resume(&mut self, ctx: &egui::Context, record: Record) {
        let id = record.id.clone();
        let role = record.role;
        let home = self.settings.home.clone();
        let (ask, answers) = recv::answers();
        let (go, turn) = tokio::sync::watch::channel(self.line_is_free());
        let mut job = Job::start(
            self.runtime(),
            ctx,
            JobView::from_record(&record),
            self.settings.notify,
            move |r, _| r.id == id,
            move |events, stop| async move {
                match role {
                    Role::Send => send::resume_send(&home, record, Some(turn), events, stop).await.map(Finish::Sent),
                    Role::Receive => recv::resume_receive(&home, record, Some(answers), events, stop).await.map(Finish::Received),
                }
            },
        );
        job.answer = Some(ask);
        if role == Role::Send {
            job.turn = Some(go);
            job.place = self.next_place();
        }
        self.jobs.insert(0, job);
        self.scan();
    }

    /// Does what a button of a transfer card asks.
    fn act(&mut self, action: CardAction, view: &Arc<Mutex<JobView>>) {
        match action {
            CardAction::First => self.first(view),
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
/// The words of the codes come from the EFF, under CC BY 3.0 US, which asks
/// for this credit wherever the list goes.
const WORD_LIST_CREDIT: &str = "The words in the codes come from the EFF Large Wordlist for Passphrases, by the Electronic Frontier Foundation, licensed under CC BY 3.0 US. Carrick uses it without its four hyphenated words.";

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

/// The arrow that opens the recent folders, and the cross that takes one off.
const RECENT_ICON: &str = "⏷";
const REMOVE_ICON: &str = "🗙";

/// Where the window remembers the folders received into lately.
fn folders_file(home: &carrick_core::home::Home) -> PathBuf {
    home.root().join("carrick-gui.folders")
}

/// The folders received into lately, newest first: one per line.
fn read_folders(home: &carrick_core::home::Home) -> Vec<PathBuf> {
    std::fs::read_to_string(folders_file(home))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .take(RECENT_FOLDERS)
        .collect()
}

fn write_folders(home: &carrick_core::home::Home, folders: &[PathBuf]) {
    let body: String = folders.iter().map(|f| format!("{}\n", f.display())).collect();
    let _ = std::fs::write(folders_file(home), body);
}

/// Whether two folders are the same one: on Windows, letter case and a
/// trailing separator do not make another folder.
fn same_folder(a: &str, b: &str) -> bool {
    let tidy = |s: &str| {
        let s = s.trim().trim_end_matches(['\\', '/']);
        if cfg!(windows) { s.to_lowercase() } else { s.to_string() }
    };
    tidy(a) == tidy(b)
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
#[cfg(test)]
mod tests {
    use super::same_folder;

    #[test]
    fn a_folder_is_the_same_with_or_without_a_final_separator() {
        assert!(same_folder("C:\\Users\\x\\Downloads", "C:\\Users\\x\\Downloads\\"));
        assert!(!same_folder("C:\\Users\\x\\Downloads", "C:\\Users\\x\\Documents"));
        #[cfg(windows)]
        assert!(same_folder("C:\\Users\\X\\downloads", "c:\\users\\x\\Downloads"));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardAction {
    /// Send this before the others in line.
    First,
    Stop,
    /// Take a finished transfer off the list of this window.
    Remove,
    /// The answer about files that already exist.
    Answer(Existing),
}

/// A send someone already has in full: it stays on offer for latecomers
/// but no longer holds up the sends behind it.
fn served(view: &JobView) -> bool {
    view.receivers.iter().any(|r| r.done)
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
            look::progress(ui, fraction, 14.0, true);
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
                        Some(Route::Direct) => look::chip(ui, "direct", look::good(ui)),
                        Some(Route::Relay) => look::chip(ui, "through a relay", look::caution(ui)),
                        None => {}
                    }
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
                    look::progress(ui, fmt::fraction(r.have, r.total), 8.0, false);
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
            ui.horizontal(|ui| {
                let mut pressed = None;
                if matches!(view.phase, Phase::Queued | Phase::Paused)
                    && ui.button("Send this first").on_hover_text("Goes before the others in line; the one sending now pauses and carries on after").clicked()
                {
                    pressed = Some(CardAction::First);
                }
                if ui.add_enabled(!view.stopping, egui::Button::new("Stop")).clicked() {
                    pressed = Some(CardAction::Stop);
                }
                pressed
            })
            .inner
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
        Phase::Queued => {
            ui.label(RichText::new("In line: it starts by itself when the send before it is done.").color(look::caution(ui)));
        }
        Phase::Paused => {
            ui.label(RichText::new("Paused: another send goes first. It carries on by itself after; its receivers wait.").color(look::caution(ui)));
        }
        Phase::Offering => {
            let waiting = view.receivers.iter().all(|r| !r.connected || r.done);
            if view.receivers.is_empty() {
                let key = if view.code.is_some() { "the code" } else { "the long code" };
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

/// One transfer of the history: what it was, when, how long it took and how
/// fast it went.
fn history_card(ui: &mut Ui, f: &Finished, now: u64) {
    look::card(ui, |ui| {
        let what = match f.role {
            Role::Send => "Sent",
            Role::Receive => "Received",
        };
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(format!("{}, {}", fmt::files(f.files), fmt::bytes(f.bytes))).color(look::weak(ui)));
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(RichText::new(format!("{what} {}", f.name)).strong().size(16.0)).truncate());
                });
            });
        });
        let seconds = f.ended.saturating_sub(f.started);
        let mut line = vec![fmt::ago(now.saturating_sub(f.ended))];
        if seconds > 0 {
            line.push(format!("took {}", fmt::took(seconds)));
            if f.moved > 0 {
                line.push(format!("average {}/s", fmt::bytes(f.moved / seconds)));
            }
        }
        match f.direct {
            Some(true) => line.push("direct".into()),
            Some(false) => line.push("through a relay".into()),
            None => {}
        }
        ui.label(line.join("  ·  "));
        let mut more = Vec::new();
        match f.role {
            Role::Receive => {
                if let Some(dest) = &f.dest {
                    more.push(format!("Saved in {}", dest.display()));
                }
            }
            Role::Send => more.push(if f.receivers == 1 {
                "1 receiver got everything".to_string()
            } else {
                format!("{} receivers got everything", f.receivers)
            }),
        }
        if f.moved < f.bytes {
            more.push(format!("{} travelled; the rest was already there", fmt::bytes(f.moved)));
        }
        ui.add(egui::Label::new(RichText::new(more.join(" · ")).small().color(look::weak(ui))).truncate());
    });
}

/// A transfer another Carrick on this computer is running: where it stands,
/// as it last wrote it. No buttons: it is stopped where it runs.
fn elsewhere_card(ui: &mut Ui, record: &Record, progress: Option<&Progress>) {
    look::card(ui, |ui| {
        let name = progress.map(|p| p.name.as_str()).filter(|n| !n.is_empty()).unwrap_or(&record.name);
        let name = if name.is_empty() { "…" } else { name };
        let files = progress.map(|p| p.files).filter(|f| *f > 0).unwrap_or(record.files);
        let total = progress.map(|p| p.total).filter(|t| *t > 0).unwrap_or(record.total_bytes);
        let what = match record.role {
            Role::Send => "Sending",
            Role::Receive => "Receiving",
        };
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if total > 0 || files > 0 {
                    ui.label(RichText::new(format!("{}, {}", fmt::files(files), fmt::bytes(total))).color(look::weak(ui)));
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(RichText::new(format!("{what} {name}")).strong().size(16.0)).truncate());
                });
            });
        });
        let terminal = progress.is_none_or(|p| p.program == "carrick");
        ui.label(
            RichText::new(if terminal {
                "In a terminal on this computer. Ctrl+C there stops it."
            } else {
                "In another Carrick window on this computer."
            })
            .color(look::weak(ui)),
        );
        let Some(p) = progress else { return };
        let now = now_secs();
        let moved = if p.receivers.is_empty() { p.have } else { p.receivers.iter().map(|r| r.have).sum() };
        let average = p.since.and_then(|since| {
            let seconds = now.saturating_sub(since);
            (seconds >= 2 && moved > p.since_have).then(|| (moved - p.since_have) as f64 / seconds as f64)
        });
        match &p.stage {
            Stage::Starting => {
                ui.label("Starting…");
            }
            Stage::Preparing => {
                ui.label(format!("Preparing: {} of {}", fmt::bytes(p.have), fmt::bytes(p.total)));
            }
            Stage::Queued => {
                ui.label("In line behind another send.");
            }
            Stage::Paused => {
                ui.label("Paused while another send goes first.");
            }
            Stage::Offering => {
                let busy = p.receivers.iter().filter(|r| r.connected && !r.done).count();
                let done = p.receivers.iter().filter(|r| r.done).count();
                ui.label(match (busy, done) {
                    (0, 0) => "Waiting for someone to enter the code.".to_string(),
                    (0, done) => format!("{done} of {} receivers have everything. Still on offer.", p.receivers.len()),
                    (1, _) => "Sending to 1 receiver".to_string(),
                    (busy, _) => format!("Sending to {busy} receivers"),
                });
            }
            Stage::Looking => {
                ui.label("Looking for the sender…");
            }
            Stage::Checking => {
                ui.label("Checking what is already here");
            }
            Stage::Receiving => {
                let mut line = format!("{} of {}", fmt::bytes(p.have), fmt::bytes(p.total));
                if let Some(speed) = average {
                    let left = p.total.saturating_sub(p.have) as f64 / speed;
                    line.push_str(&format!("  ·  {} left", fmt::eta(Duration::from_secs_f64(left))));
                }
                ui.label(line);
            }
            Stage::Waiting { reason } => {
                ui.label(RichText::new(format!("Interrupted: {reason}. Trying again by itself…")).color(look::caution(ui)));
            }
            Stage::Asking => {
                ui.label(RichText::new("Some files already exist with different content: it is waiting for an answer there.").color(look::caution(ui)));
            }
            Stage::Saving { done, total } => {
                ui.label(format!("Saving files: {done} of {total}"));
            }
        }
        let fraction = match &p.stage {
            Stage::Preparing | Stage::Checking | Stage::Receiving | Stage::Waiting { .. } => Some(fmt::fraction(p.have, p.total)),
            Stage::Saving { done, total } => Some(fmt::fraction(*done, *total)),
            _ => None,
        };
        if let Some(fraction) = fraction {
            look::progress(ui, fraction, 14.0, true);
        }
        if record.role == Role::Receive {
            match p.direct {
                Some(true) => route_line(ui, Route::Direct),
                Some(false) => route_line(ui, Route::Relay),
                None => {}
            }
        }
        for r in &p.receivers {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Receiver {}", r.id)).monospace());
                match r.direct {
                    Some(true) => look::chip(ui, "direct", look::good(ui)),
                    Some(false) => look::chip(ui, "through a relay", look::caution(ui)),
                    None => {}
                }
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
            if r.connected && !r.done {
                look::progress(ui, fmt::fraction(r.have, r.total), 8.0, false);
            }
        }
        let mut parts = Vec::new();
        if let Some(speed) = p.speed {
            parts.push(format!("Now {}/s", fmt::bytes(speed as u64)));
        }
        if let Some(speed) = average {
            parts.push(format!("average {}/s", fmt::bytes(speed as u64)));
        }
        if let Some(since) = p.since {
            parts.push(format!("{} so far", fmt::eta(Duration::from_secs(now.saturating_sub(since)))));
        }
        if !parts.is_empty() {
            let mut text = parts.join("  ·  ");
            if let Some(first) = text.get(0..1) {
                text = first.to_uppercase() + &text[1..];
            }
            ui.add(egui::Label::new(RichText::new(text).color(look::weak(ui))).truncate());
        }
    });
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
    ui.horizontal_wrapped(|ui| match route {
        Route::Direct => look::chip(ui, "Direct, computer to computer", look::good(ui)),
        Route::Relay => {
            look::chip(ui, "Through a relay", look::caution(ui));
            ui.label(
                RichText::new("still encrypted end to end. The free public relays are slow on purpose.")
                    .small()
                    .color(look::weak(ui)),
            );
        }
    });
}
