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

    // What was chosen first goes by ticket, and is caught half way.
    tick(&mut a, "Use a ticket instead of a code (works without Internet)");
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

    // Stopped, the send waits under Unfinished.
    button(&mut a, "Stop");
    wait(&mut a, "the send to stop", |app| sending(app).phase.is_over());
    wait(&mut a, "the unfinished list", |app| app.unfinished.len() == 1);
    shoot(&mut a, "12-transfers-unfinished");
    tab(&mut r, "Transfers");
    shoot(&mut r, "13-transfers-receiver");

    for w in [&mut c, &mut d] {
        tab(w, "Transfers");
        button(w, "Stop");
        wait(w, "a code send to stop", |app| sending(app).phase.is_over());
    }
}
