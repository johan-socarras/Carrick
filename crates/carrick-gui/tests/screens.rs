//! Draws every screen of the window to a PNG, from real transfers between
//! windows in this process, so they can be looked at without opening the
//! window on anyone's desktop.
//!
//! Ignored by default: it needs a graphics adapter (wgpu picks a software
//! one when there is one), writes about 1.5 GB of temporary files and
//! leaves PNGs behind.
//!
//! `CARRICK_SCREENS=<folder> cargo test -p carrick-gui --test screens -- --ignored`

mod common;

use std::path::{Path, PathBuf};

use common::*;
use eframe::egui;
use egui_kittest::{kittest::Queryable, Harness};
use carrick_core::home::Home;
use carrick_gui::{job::Phase, App, Settings};

fn folder() -> PathBuf {
    let dir = std::env::var_os("CARRICK_SCREENS")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("carrick-screens"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A window that can be drawn, in the given theme.
fn drawable(home: &Path, theme: egui::Theme) -> Window {
    let home = Home::at(home).unwrap();
    // The window starts with the theme it last saved: this one.
    let word = if theme == egui::Theme::Dark { "dark" } else { "light" };
    std::fs::write(home.root().join("carrick-gui.theme"), word).unwrap();
    let settings = Settings {
        home,
        net: offline(),
        notify: false,
        default_dest: None,
        threads: 2,
    };
    Harness::builder()
        // The harness keeps 8 points around the window; they are cut below.
        .with_size(egui::vec2(640.0 + 16.0, 720.0 + 16.0))
        .with_theme(theme)
        .with_context_setup(carrick_gui::install)
        .wgpu()
        .build_ui_state(|ui, app: &mut App| app.show(ui), App::new(settings))
}

fn shoot(w: &mut Window, name: &str) {
    settle(w);
    let image = w.render().expect("draw the window");
    let image = image::imageops::crop_imm(&image, 8, 8, image.width() - 16, image.height() - 16).to_image();
    let path = folder().join(format!("{name}.png"));
    image.save(&path).unwrap();
    println!("wrote {}", path.display());
}

#[test]
#[ignore]
fn every_screen_to_an_image() {
    let tmp = tempfile::tempdir().unwrap();
    let source = sample_tree(&tmp.path().join("source"));
    // One big file, so the progress can be caught half way.
    let video = tmp.path().join("source/holiday.mov");
    fill(&video, 512 * 1024 * 1024, 7);
    let dest = tmp.path().join("Downloads");
    std::fs::create_dir_all(&dest).unwrap();

    // Send: empty, then with a folder and a file dropped on it.
    let mut a = drawable(&tmp.path().join("home-a"), egui::Theme::Light);
    shoot(&mut a, "01-send");
    drop_on(&mut a, &[source.clone(), video.clone()]);
    shoot(&mut a, "02-send-chosen");

    // A four-word code, in both themes. Without a relay nothing reaches it
    // from outside, but the code is real and so is the screen.
    let mut c = drawable(&tmp.path().join("home-c"), egui::Theme::Light);
    drop_on(&mut c, &[source.clone()]);
    button(&mut c, "Start sending");
    wait(&mut c, "the code", |app| sending(app).code.is_some());
    shoot(&mut c, "03-send-code");
    let code = sending(c.state()).code.unwrap();
    let mut d = drawable(&tmp.path().join("home-d"), egui::Theme::Dark);
    drop_on(&mut d, &[source.clone()]);
    button(&mut d, "Start sending");
    wait(&mut d, "the code", |app| sending(app).code.is_some());
    shoot(&mut d, "04-send-code-dark");

    // Receive: empty, then filled in with that code.
    let mut b = drawable(&tmp.path().join("home-b"), egui::Theme::Light);
    tab(&mut b, "Receive");
    shoot(&mut b, "05-receive");
    type_into(&mut b, "Code", &code);
    type_into(&mut b, "Save to", &dest.display().to_string());
    shoot(&mut b, "06-receive-filled");
    // Folders used before are offered from the arrow (shown with made-up
    // paths, so no folder of the computer that draws them appears).
    b.state_mut().receive.dest = r"C:\Users\you\Downloads".into();
    b.state_mut().receive.recent = vec![
        PathBuf::from(r"C:\Users\you\Downloads"),
        PathBuf::from(r"\\fileserver\shared\videos"),
        PathBuf::from(r"D:\Projects\2026"),
    ];
    button(&mut b, "⏷");
    shoot(&mut b, "06b-receive-recent-folders");
    b.key_press(egui::Key::Escape);
    settle(&mut b);

    // What was chosen first goes by ticket, and is caught half way.
    tick(&mut a, "No Internet? Use a long code instead of four words");
    button(&mut a, "Start sending");
    wait(&mut a, "the ticket", |app| sending(app).ticket.is_some());
    shoot(&mut a, "07-send-ticket");
    let ticket = sending(a.state()).ticket.unwrap();
    let mut r = drawable(&tmp.path().join("home-r"), egui::Theme::Light);
    tab(&mut r, "Receive");
    type_into(&mut r, "Code", &ticket);
    type_into(&mut r, "Save to", &dest.display().to_string());
    button(&mut r, "Start receiving");
    wait_both(&mut a, &mut r, "a third of the files", |_, r| {
        let v = receiving(r);
        v.phase.is_over() || (v.phase == Phase::Receiving && v.fraction().unwrap_or(0.0) > 0.3)
    });
    shoot(&mut r, "08-receive-progress");
    tab(&mut a, "Transfers");
    // Receivers report to the sender once a second.
    wait_both(&mut r, &mut a, "the sender to see progress", |_, a| sending(a).receivers.iter().any(|x| x.have > 0));
    shoot(&mut a, "09-transfers-sending");
    wait_both(&mut a, &mut r, "the files to arrive", |_, r| receiving(r).phase.is_over());
    assert_eq!(receiving(r.state()).phase, Phase::Done);
    shoot(&mut r, "10-receive-done");
    wait_both(&mut r, &mut a, "the sender to see it", |_, a| sending(a).receivers.iter().any(|x| x.done));
    shoot(&mut a, "11-transfers-received");

    // A second receiver whose folder already holds a different file asks.
    let dest2 = tmp.path().join("dest2");
    std::fs::create_dir_all(&dest2).unwrap();
    std::fs::write(dest2.join("holiday.mov"), b"another film").unwrap();
    let mut q = drawable(&tmp.path().join("home-q"), egui::Theme::Light);
    tab(&mut q, "Receive");
    type_into(&mut q, "Code", &ticket);
    type_into(&mut q, "Save to", &dest2.display().to_string());
    shoot(&mut q, "14-receive-existing-choice");
    button(&mut q, "Start receiving");
    wait_both(&mut a, &mut q, "the question", |_, q| receiving(q).asking.is_some() || receiving(q).phase.is_over());
    shoot(&mut q, "15-receive-asking");
    button(&mut q, "Keep both");
    wait_both(&mut a, &mut q, "the files to arrive", |_, q| receiving(q).phase.is_over());
    shoot(&mut q, "16-receive-kept-both");

    // About, in light and in dark.
    tab(&mut q, "About");
    shoot(&mut q, "17-about");
    q.get_by_label(&egui::ThemePreference::Dark.icon().to_string()).click_accesskit();
    shoot(&mut q, "18-about-dark");

    // Someone has it all: stopped, the send ends and goes to the history.
    button(&mut a, "Stop");
    wait(&mut a, "the send to stop", |app| sending(app).phase.is_over());
    a.state_mut().scan();
    button(&mut a, "Show history");
    shoot(&mut a, "12-transfers-history");
    tab(&mut r, "Transfers");
    button(&mut r, "Show history");
    shoot(&mut r, "13-transfers-receiver-history");

    // A second send while one is on offer waits in line; then it goes first.
    tab(&mut c, "Send");
    button(&mut c, "Send something else");
    drop_on(&mut c, &[video.clone()]);
    button(&mut c, "Start sending");
    wait(&mut c, "the second send to be in line", |app| sending(app).phase == Phase::Queued);
    shoot(&mut c, "19-send-queued");
    tab(&mut c, "Transfers");
    shoot(&mut c, "20-transfers-line");
    button(&mut c, "Send this first");
    wait(&mut c, "the second to go and the first to pause", |app| {
        let phase = |i: usize| app.jobs[i].view.lock().unwrap().phase.clone();
        phase(0) == Phase::Offering && phase(1) == Phase::Paused
    });
    shoot(&mut c, "21-transfers-first");

    // A receive that a terminal runs, seen from the window.
    let t_home = tmp.path().join("home-t");
    let mut t = drawable(&t_home, egui::Theme::Light);
    let held = terminal_receive(&Home::at(&t_home).unwrap(), &dest);
    tab(&mut t, "Transfers");
    shoot(&mut t, "22-transfers-terminal");
    drop(held);

    for w in [&mut c, &mut d] {
        w.state().jobs.iter().for_each(|j| j.stop());
        wait(w, "the sends to stop", |app| app.jobs.iter().all(|j| !j.is_running()));
    }
}

/// What a terminal holds and writes while it receives: the lock that says
/// it runs, and where it stands.
fn terminal_receive(home: &Home, dest: &Path) -> carrick_core::running::Claim {
    use carrick_core::{
        home::{now_secs, Record, Role, State},
        running::{Progress, Stage},
    };
    let gib = 1024 * 1024 * 1024;
    let mut record = Record {
        id: "r0123456789abcde".into(),
        role: Role::Receive,
        state: State::Active,
        created: now_secs(),
        updated: 0,
        code: None,
        key: Some("ck1example".into()),
        endpoint_secret: "00".repeat(32),
        auth_key: "00".repeat(32),
        root: None,
        name: "holiday-videos-2026".into(),
        total_bytes: 9 * gib,
        files: 21,
        sources: Vec::new(),
        dest: Some(dest.to_path_buf()),
        store: dest.join(".carrick-r0123456789abcde"),
        relay: None,
        receivers_wanted: None,
        finished_receivers: Vec::new(),
        overwrite: false,
        existing: None,
        saved: Vec::new(),
    };
    home.save(&mut record).unwrap();
    let held = home.claim(&record.id).unwrap();
    home.write_progress(
        &record.id,
        &Progress {
            program: "carrick".into(),
            stage: Stage::Receiving,
            name: record.name.clone(),
            files: 21,
            have: 3 * gib,
            total: 9 * gib,
            direct: Some(true),
            speed: Some(2.5 * 1024.0 * 1024.0),
            since: Some(now_secs() - 1200),
            since_have: 0,
            receivers: Vec::new(),
            updated: now_secs(),
        },
    );
    held
}
