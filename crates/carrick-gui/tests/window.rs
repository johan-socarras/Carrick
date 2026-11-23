//! Whole transfers made only through the window: two windows in this
//! process, each with its own data folder, pressed from code.
//!
//! They use a ticket and no relay, so they need no server and no Internet.
//! The one test with a four-word code needs both and is ignored by default.

mod common;

use common::*;
use eframe::egui::{self, Role};
use egui_kittest::kittest::{NodeT, Queryable};
use carrick_core::{
    events::Route,
    home::{now_secs, Home, Record, Role as Way, State},
    net::NetOptions,
};
use carrick_gui::{job::Phase, Screen};

#[test]
fn a_folder_goes_from_one_window_to_another() {
    let tmp = tempfile::tempdir().unwrap();
    let source = sample_tree(&tmp.path().join("source"));
    let dest = tmp.path().join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    let mut sender = window(&tmp.path().join("home-a"), offline());
    let mut receiver = window(&tmp.path().join("home-b"), offline());

    // Sender: the folder is dropped on the window, and a ticket asked for.
    assert!(shows(&sender, "Drop files or a folder here"));
    assert!(sender.get_by_role_and_label(Role::Button, "Start sending").accesskit_node().is_disabled());
    drop_on(&mut sender, &[source.clone()]);
    assert_eq!(sender.state().send.paths, vec![source.clone()]);
    assert!(shows(&sender, "photos"));
    tick(&mut sender, "Use a ticket instead of a code (works without Internet)");
    button(&mut sender, "Start sending");
    wait(&mut sender, "the ticket", |app| sending(app).ticket.is_some());
    assert!(shows(&sender, "paste this ticket"));
    let ticket = copy_with(&mut sender, "Copy");
    assert_eq!(Some(ticket.clone()), sending(sender.state()).ticket);
    settle(&mut sender);
    assert!(shows(&sender, "Copied"));

    // Receiver: the ticket is pasted and a folder typed in.
    tab(&mut receiver, "Receive");
    assert_eq!(receiver.state().screen, Screen::Receive);
    type_into(&mut receiver, "Code", &ticket);
    type_into(&mut receiver, "Save to", &dest.display().to_string());
    button(&mut receiver, "Start receiving");
    wait_both(&mut sender, &mut receiver, "the files to arrive", |_, b| receiving(b).phase.is_over());
    let view = receiving(receiver.state());
    assert_eq!(view.phase, Phase::Done, "{view:?}");
    assert_eq!(view.name, "photos");
    assert_eq!(view.files, 5);
    assert!(shows(&receiver, "Done: saved in"));
    assert_eq!(snapshot(&tmp.path().join("source")), snapshot(&dest));
    assert!(dest.join("photos/empty-folder").is_dir());

    // The sender saw the receiver connect directly and get everything.
    wait_both(&mut receiver, &mut sender, "the sender to see it arrive", |_, a| {
        sending(a).receivers.iter().any(|r| r.done)
    });
    let view = sending(sender.state());
    assert_eq!(view.receivers.len(), 1);
    assert_eq!(view.receivers[0].route, Some(Route::Direct));
    assert_eq!(view.phase, Phase::Offering, "stays on offer until stopped");

    // Both show it under Transfers.
    tab(&mut receiver, "Transfers");
    assert!(shows(&receiver, "Done: saved in"));
    tab(&mut sender, "Transfers");
    assert!(shows(&sender, "has everything"));

    // Stopped, the send is offered from Unfinished, and Continue offers it again.
    button(&mut sender, "Stop");
    wait(&mut sender, "the send to stop", |app| sending(app).phase == Phase::Stopped);
    wait(&mut sender, "the send to be listed as unfinished", |app| app.unfinished.len() == 1);
    assert!(shows(&sender, "Unfinished"));
    button(&mut sender, "Continue");
    wait(&mut sender, "the send to be on offer again", |app| {
        app.jobs[0].view.lock().unwrap().phase == Phase::Offering
    });
    assert!(sender.state().unfinished.is_empty(), "a transfer running here is not listed as unfinished");
    assert_eq!(sender.state().jobs.len(), 2);
    button(&mut sender, "Stop");
    wait(&mut sender, "the continued send to stop", |app| {
        app.jobs[0].view.lock().unwrap().phase == Phase::Stopped
    });
}

#[test]
fn a_mistyped_code_is_explained() {
    let tmp = tempfile::tempdir().unwrap();
    let mut w = window(&tmp.path().join("home"), offline());
    tab(&mut w, "Receive");
    assert!(w.get_by_role_and_label(Role::Button, "Start receiving").accesskit_node().is_disabled());
    type_into(&mut w, "Code", "apple river stone");
    type_into(&mut w, "Save to", &tmp.path().display().to_string());
    button(&mut w, "Start receiving");
    wait(&mut w, "the refusal", |app| receiving(app).phase.is_over());
    assert!(matches!(receiving(w.state()).phase, Phase::Failed(_)));
    assert!(shows(&w, "Failed: a code has 4 words"));
}

#[test]
fn dropping_files_on_another_screen_goes_to_send() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("notes.txt");
    std::fs::write(&file, "hola").unwrap();
    let mut w = window(&tmp.path().join("home"), offline());
    tab(&mut w, "Transfers");
    assert!(shows(&w, "Nothing here yet"));
    drop_on(&mut w, &[file.clone(), file.clone()]);
    assert_eq!(w.state().screen, Screen::Send);
    assert_eq!(w.state().send.paths, vec![file], "the same file is listed once");
    button(&mut w, "Remove");
    assert!(w.state().send.paths.is_empty());
}

/// The real thing: a four-word code, found through n0's lookup service.
/// Needs Internet: `cargo test -p carrick-gui --test window -- --ignored`.
#[test]
#[ignore]
fn a_four_word_code_works_over_the_internet() {
    let tmp = tempfile::tempdir().unwrap();
    let source = sample_tree(&tmp.path().join("source"));
    let dest = tmp.path().join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    let net = NetOptions::default();
    let mut sender = window(&tmp.path().join("home-a"), net.clone());
    let mut receiver = window(&tmp.path().join("home-b"), net);

    drop_on(&mut sender, &[source]);
    button(&mut sender, "Start sending");
    wait(&mut sender, "the code", |app| sending(app).code.is_some());
    let code = copy_with(&mut sender, "Copy");
    assert_eq!(code.split('-').count(), 4, "{code}");
    assert!(shows(&sender, &code));

    tab(&mut receiver, "Receive");
    type_into(&mut receiver, "Code", &code);
    type_into(&mut receiver, "Save to", &dest.display().to_string());
    button(&mut receiver, "Start receiving");
    wait_both(&mut sender, &mut receiver, "the files to arrive", |_, b| receiving(b).phase.is_over());
    let view = receiving(receiver.state());
    assert_eq!(view.phase, Phase::Done, "{view:?}");
    assert_eq!(snapshot(&tmp.path().join("source")), snapshot(&dest));
    println!("route seen by the receiver: {:?}", view.route);

    wait_both(&mut receiver, &mut sender, "the sender to see it arrive", |_, a| {
        sending(a).receivers.iter().any(|r| r.done)
    });
    println!("route seen by the sender: {:?}", sending(sender.state()).receivers[0].route);
    tab(&mut sender, "Transfers");
    button(&mut sender, "Stop");
    wait(&mut sender, "the send to stop", |app| sending(app).phase == Phase::Stopped);
}

#[test]
fn an_unfinished_transfer_with_a_long_name_fits_and_can_be_forgotten() {
    let tmp = tempfile::tempdir().unwrap();
    let home_dir = tmp.path().join("home");
    let home = Home::at(&home_dir).unwrap();
    let dest = tmp.path().join("dest");
    let store = dest.join(".carrick-0123456789");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(dest.join("already-here.bin"), b"a file that arrived whole").unwrap();
    let long = "Camera 59_EA15_EA15_20261121150959_20261121153144_6580762 and more words that cannot fit in one line.mp4";
    let mut record = Record {
        id: "0123456789".into(),
        role: Way::Receive,
        state: State::Active,
        created: now_secs(),
        updated: 0,
        code: None,
        key: Some("ck1example".into()),
        endpoint_secret: "00".repeat(32),
        auth_key: "00".repeat(32),
        root: None,
        name: long.into(),
        total_bytes: 114 * 1024 * 1024 * 1024,
        files: 123,
        sources: Vec::new(),
        dest: Some(dest.clone()),
        store: store.clone(),
        relay: None,
        receivers_wanted: None,
        finished_receivers: Vec::new(),
        overwrite: false,
        existing: None,
        saved: vec![0],
    };
    home.save(&mut record).unwrap();

    let mut w = window(&home_dir, offline());
    tab(&mut w, "Transfers");
    assert!(shows(&w, "Unfinished"));
    // Nothing of the card goes past the window: the name is cut short.
    let width = 640.0;
    for name in ["Continue", "Forget"] {
        let right = w.get_by_role_and_label(Role::Button, name).rect().right();
        assert!(right <= width, "{name} ends at {right}, past the window ({width})");
    }

    // Forget asks first; Keep leaves it as it was.
    button(&mut w, "Forget");
    assert!(shows(&w, "Forget it?"));
    button(&mut w, "Keep");
    assert!(!shows(&w, "Forget it?"));
    assert!(home.load("0123456789").unwrap().is_some());

    // Forget, confirmed: the record and the loose pieces go; what had
    // arrived whole stays in the destination.
    button(&mut w, "Forget");
    button(&mut w, "Forget");
    assert!(home.load("0123456789").unwrap().is_none());
    assert!(!store.exists(), "the pieces were not deleted");
    assert!(dest.join("already-here.bin").exists(), "a finished file was deleted");
    assert!(!shows(&w, "Unfinished"));
}

#[test]
fn the_window_asks_about_files_that_already_exist() {
    let tmp = tempfile::tempdir().unwrap();
    let source = sample_tree(&tmp.path().join("source"));
    let dest = tmp.path().join("dest");
    // A different file where one of the incoming ones goes.
    std::fs::create_dir_all(dest.join("photos")).unwrap();
    std::fs::write(dest.join("photos/big.bin"), b"a different file that must stay").unwrap();
    let mut sender = window(&tmp.path().join("home-a"), offline());
    let mut receiver = window(&tmp.path().join("home-b"), offline());

    drop_on(&mut sender, &[source.clone()]);
    tick(&mut sender, "Use a ticket instead of a code (works without Internet)");
    button(&mut sender, "Start sending");
    wait(&mut sender, "the ticket", |app| sending(app).ticket.is_some());
    let ticket = copy_with(&mut sender, "Copy");

    tab(&mut receiver, "Receive");
    assert!(shows(&receiver, "If a file already exists:"));
    assert!(shows(&receiver, "Ask me"), "asking is the default");
    type_into(&mut receiver, "Code", &ticket);
    type_into(&mut receiver, "Save to", &dest.display().to_string());
    button(&mut receiver, "Start receiving");
    wait_both(&mut sender, &mut receiver, "the question", |_, b| receiving(b).asking.is_some());
    assert!(shows(&receiver, "1 file already exists here with different content"), "{:?}", painted(&receiver));
    assert!(shows(&receiver, "The copy waits until you choose."));

    button(&mut receiver, "Skip them");
    wait_both(&mut sender, &mut receiver, "the files to arrive", |_, b| receiving(b).phase.is_over());
    let view = receiving(receiver.state());
    assert_eq!(view.phase, Phase::Done, "{view:?}");
    assert!(view.asking.is_none());
    assert_eq!(std::fs::read(dest.join("photos/big.bin")).unwrap(), b"a different file that must stay");
    assert_eq!(std::fs::read(dest.join("photos/nested/other.bin")).unwrap(), std::fs::read(source.join("nested/other.bin")).unwrap());
}

#[test]
fn light_or_dark_is_remembered_and_about_shows_the_licence() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let mut w = window(&home, offline());
    settle(&mut w);
    for (theme, word) in [(egui::ThemePreference::Light, "light"), (egui::ThemePreference::Dark, "dark")] {
        w.get_by_label(&theme.icon().to_string()).click_accesskit();
        settle(&mut w);
        assert_eq!(w.ctx.options(|o| o.theme_preference), theme);
        assert_eq!(std::fs::read_to_string(home.join("carrick-gui.theme")).unwrap(), word);
    }
    // A new window starts as the last one was left.
    let mut again = window(&home, offline());
    settle(&mut again);
    assert_eq!(again.ctx.options(|o| o.theme_preference), egui::ThemePreference::Dark);

    tab(&mut again, "About");
    assert!(shows(&again, &format!("Version {}", env!("CARGO_PKG_VERSION"))));
    assert!(shows(&again, "licensed under the MIT licence or the Apache License 2.0"), "{:?}", painted(&again));
    assert!(shows(&again, "github.com/johan-socarras/Carrick"));
    assert!(shows(&again, "THIRD-PARTY-LICENSES.txt"));
}