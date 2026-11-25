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
    running::{Progress, Stage},
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
    tick(&mut sender, "No Internet? Use a long code instead of four words");
    button(&mut sender, "Start sending");
    wait(&mut sender, "the ticket", |app| sending(app).ticket.is_some());
    assert!(shows(&sender, "paste this long code"));
    let ticket = copy_with(&mut sender, "Copy");
    assert_eq!(Some(ticket.clone()), sending(sender.state()).ticket);
    settle(&mut sender);
    assert!(shows(&sender, "Copied"));
    // And the whole command, for a terminal.
    assert_eq!(copy_with(&mut sender, "Copy command"), format!("carrick receive {ticket}"));
    settle(&mut sender);
    assert!(shows(&sender, "Command copied"));

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

    // Under Transfers only what is being copied: the receive has ended and is
    // in the history; the send is still on offer.
    tab(&mut receiver, "Transfers");
    assert!(shows(&receiver, "Nothing is being copied right now."), "{:?}", painted(&receiver));
    button(&mut receiver, "Show history");
    assert_eq!(receiver.state().history.len(), 1);
    for text in ["Received photos", "5 files", "Saved in", "direct"] {
        assert!(shows(&receiver, text), "{text} is not in the history: {:?}", painted(&receiver));
    }
    tab(&mut sender, "Transfers");
    assert!(shows(&sender, "has everything"));

    // Someone has everything: stopping ends the send, and it is not left
    // under Unfinished to be offered again.
    button(&mut sender, "Stop");
    wait(&mut sender, "the send to end", |app| sending(app).phase.is_over());
    assert_eq!(sending(sender.state()).phase, Phase::Done);
    assert!(!shows(&sender, "has everything"), "an ended send is still under Transfers");
    tab(&mut sender, "Send");
    assert!(shows(&sender, "Done: 1 receiver has everything."), "{:?}", painted(&sender));
    tab(&mut sender, "Transfers");
    sender.state_mut().scan();
    settle(&mut sender);
    assert!(sender.state().unfinished.is_empty(), "a finished send is listed as unfinished");
    assert!(!shows(&sender, "Unfinished"));
    let home = Home::at(tmp.path().join("home-a")).unwrap();
    assert!(home.list().unwrap().iter().all(|r| r.state == State::Done));
    tab(&mut sender, "Transfers");
    button(&mut sender, "Show history");
    assert!(shows(&sender, "Sent photos"), "{:?}", painted(&sender));
    assert!(shows(&sender, "1 receiver got everything"));
}

/// A second send while one is on offer waits in line, and "Send this first"
/// lets it go ahead: the first one pauses, and carries on when it is stopped.
#[test]
fn a_second_send_waits_in_line_and_can_go_first() {
    let tmp = tempfile::tempdir().unwrap();
    let first = sample_tree(&tmp.path().join("a"));
    let second = tmp.path().join("b/notes.txt");
    std::fs::create_dir_all(second.parent().unwrap()).unwrap();
    std::fs::write(&second, "hola").unwrap();
    let mut w = window(&tmp.path().join("home"), offline());
    drop_on(&mut w, &[first]);
    tick(&mut w, "No Internet? Use a long code instead of four words");
    button(&mut w, "Start sending");
    wait(&mut w, "the first ticket", |app| sending(app).ticket.is_some());

    button(&mut w, "Send something else");
    drop_on(&mut w, &[second]);
    button(&mut w, "Start sending");
    wait(&mut w, "the second send to be in line", |app| sending(app).phase == Phase::Queued);
    assert!(shows(&w, "notes.txt is in line"), "{:?}", painted(&w));
    assert!(sending(w.state()).ticket.is_none(), "a send in line has no ticket yet");

    tab(&mut w, "Transfers");
    assert!(shows(&w, "In line: it starts by itself"));
    button(&mut w, "Send this first");
    let phase = |app: &carrick_gui::App, i: usize| app.jobs[i].view.lock().unwrap().phase.clone();
    wait(&mut w, "the second to go and the first to pause", |app| phase(app, 0) == Phase::Offering && phase(app, 1) == Phase::Paused);
    assert!(shows(&w, "Paused: another send goes first."));

    // Stopping the one in front lets the paused one carry on.
    w.state().jobs[0].stop();
    wait(&mut w, "the first to carry on", |app| phase(app, 1) == Phase::Offering);
    w.state().jobs[1].stop();
    wait(&mut w, "the first to stop", |app| phase(app, 1).is_over());
}

#[test]
fn a_send_stopped_before_anyone_has_it_can_be_continued() {
    let tmp = tempfile::tempdir().unwrap();
    let source = sample_tree(&tmp.path().join("source"));
    let mut sender = window(&tmp.path().join("home"), offline());
    drop_on(&mut sender, &[source]);
    tick(&mut sender, "No Internet? Use a long code instead of four words");
    button(&mut sender, "Start sending");
    wait(&mut sender, "the ticket", |app| sending(app).ticket.is_some());

    // Stopped, the send is offered from Unfinished, and Continue offers it again.
    tab(&mut sender, "Transfers");
    button(&mut sender, "Stop");
    wait(&mut sender, "the send to stop", |app| sending(app).phase == Phase::Stopped);
    wait(&mut sender, "the send to be listed as unfinished", |app| app.unfinished.len() == 1);
    assert!(shows(&sender, "Unfinished"));
    button(&mut sender, "Continue");
    wait(&mut sender, "the send to be on offer again", |app| {
        app.jobs[0].view.lock().unwrap().phase == Phase::Offering
    });
    assert!(sender.state().unfinished.is_empty(), "a transfer running here is not listed as unfinished");
    assert!(sender.state().elsewhere.is_empty(), "a transfer this window runs is not listed as running elsewhere");
    assert_eq!(sender.state().jobs.len(), 2);
    button(&mut sender, "Stop");
    wait(&mut sender, "the continued send to stop", |app| {
        app.jobs[0].view.lock().unwrap().phase == Phase::Stopped
    });
}

/// A receive the terminal runs, seen from the window on the same computer:
/// shown with where it stands, and never offered to continue while it runs.
#[test]
fn a_transfer_running_in_a_terminal_is_shown_and_not_offered_to_continue() {
    let tmp = tempfile::tempdir().unwrap();
    let home_dir = tmp.path().join("home");
    let home = Home::at(&home_dir).unwrap();
    let dest = tmp.path().join("dest");
    let mut record = receive_record("0123456789", "holiday-videos-2026", &dest);
    home.save(&mut record).unwrap();

    // What a terminal holds and writes while it receives.
    let held = home.claim(&record.id).unwrap();
    let gib = 1024 * 1024 * 1024;
    home.write_progress(
        &record.id,
        &Progress {
            program: "carrick".into(),
            stage: Stage::Receiving,
            name: "holiday-videos-2026".into(),
            files: 21,
            have: 3 * gib,
            total: 9 * gib,
            direct: Some(true),
            speed: Some(2.5 * 1024.0 * 1024.0),
            since: Some(now_secs() - 600),
            since_have: 0,
            receivers: Vec::new(),
            updated: now_secs(),
        },
    );

    let mut w = window(&home_dir, offline());
    tab(&mut w, "Transfers");
    assert_eq!(w.state().elsewhere.len(), 1);
    assert!(w.state().unfinished.is_empty(), "a running transfer is listed as unfinished");
    for text in ["In progress", "Receiving holiday-videos-2026", "In a terminal on this computer", "3.00 GiB of 9.00 GiB", "Direct, computer to computer", "Now 2.50 MiB/s"] {
        assert!(shows(&w, text), "{text} is not shown: {:?}", painted(&w));
    }
    assert!(!shows(&w, "Unfinished"));
    assert!(w.query_by_role_and_label(Role::Button, "Continue").is_none(), "Continue offered on a running transfer");
    assert!(w.query_by_role_and_label(Role::Button, "Forget").is_none());

    // The terminal ends (closed, or cut short): now it is unfinished. The
    // window looks again every second, as it does for real. On Linux a
    // process another test starts at that moment can keep the lock for an
    // instant (a new process holds copies of the open files until it runs
    // its program), so one look right away is not enough.
    drop(held);
    wait(&mut w, "the window to see the terminal gone", |app| app.elsewhere.is_empty());
    assert_eq!(w.state().unfinished.len(), 1);
    assert!(shows(&w, "Unfinished"));
    assert!(w.query_by_role_and_label(Role::Button, "Continue").is_some());
}

/// The record of a receive left half way, as the core writes it.
fn receive_record(id: &str, name: &str, dest: &std::path::Path) -> Record {
    Record {
        id: id.into(),
        role: Way::Receive,
        state: State::Active,
        created: now_secs(),
        updated: 0,
        code: None,
        key: Some("ck1example".into()),
        endpoint_secret: "00".repeat(32),
        auth_key: "00".repeat(32),
        root: None,
        name: name.into(),
        total_bytes: 114 * 1024 * 1024 * 1024,
        files: 123,
        sources: Vec::new(),
        dest: Some(dest.to_path_buf()),
        store: dest.join(format!(".carrick-{id}")),
        relay: None,
        receivers_wanted: None,
        finished_receivers: Vec::new(),
        overwrite: false,
        existing: None,
        saved: vec![0],
    }
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

/// "Save to" opens with the folder used last time, also in a new window, and
/// offers the last five, newest first; the cross takes one off the list.
#[test]
fn save_to_remembers_the_recent_folders() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let folders: Vec<std::path::PathBuf> = (1..=7).map(|n| tmp.path().join(format!("folder {n}"))).collect();
    let mut w = window(&home, offline());
    tab(&mut w, "Receive");
    for folder in &folders {
        w.state_mut().receive.dest = folder.display().to_string();
        w.state_mut().receive.code = "apple river stone".into();
        settle(&mut w);
        button(&mut w, "Start receiving");
        wait(&mut w, "the refusal", |app| receiving(app).phase.is_over());
        button(&mut w, "Receive something else");
    }
    // The same folder again moves to the front instead of repeating.
    w.state_mut().receive.dest = folders[4].display().to_string();
    w.state_mut().receive.code = "apple river stone".into();
    settle(&mut w);
    button(&mut w, "Start receiving");
    wait(&mut w, "the refusal", |app| receiving(app).phase.is_over());

    // A new window, as after closing Carrick or a restart.
    let mut again = window(&home, offline());
    tab(&mut again, "Receive");
    let recent = again.state().receive.recent.clone();
    assert_eq!(recent.len(), 5, "{recent:?}");
    assert_eq!(again.state().receive.dest, folders[4].display().to_string());
    assert_eq!(recent[0], folders[4]);
    assert_eq!(recent[1..], [folders[6].clone(), folders[5].clone(), folders[3].clone(), folders[2].clone()]);

    // Picked from the list.
    button(&mut again, "⏷");
    assert!(shows(&again, "Recent folders"), "{:?}", painted(&again));
    again.get_by_label(&folders[3].display().to_string()).click();
    settle(&mut again);
    assert_eq!(again.state().receive.dest, folders[3].display().to_string());

    // Taken off the list: the first cross is the newest folder's.
    button(&mut again, "⏷");
    again.query_all_by_label("🗙").next().expect("a cross for each folder").click();
    settle(&mut again);
    assert_eq!(again.state().receive.recent.len(), 4);
    assert_eq!(window(&home, offline()).state().receive.recent.len(), 4, "the removal is kept");
}

#[test]
fn dropping_files_on_another_screen_goes_to_send() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("notes.txt");
    std::fs::write(&file, "hola").unwrap();
    let mut w = window(&tmp.path().join("home"), offline());
    // The window's name is in its title bar, not again next to the tabs.
    assert!(painted(&w).iter().all(|t| t != "Carrick"), "{:?}", painted(&w));
    tab(&mut w, "Transfers");
    assert!(shows(&w, "Nothing is being copied right now."));
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
    tick(&mut sender, "No Internet? Use a long code instead of four words");
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
    assert!(shows(&again, "EFF Large Wordlist for Passphrases"));
    assert!(shows(&again, "CC BY 3.0 US"));

    // The links ask the system to open them (eframe's "links" feature then
    // hands the address to the browser; without it a click did nothing).
    for (label, url) in [
        ("eff.org/dice", "https://www.eff.org/dice"),
        ("CC BY 3.0 US", "https://creativecommons.org/licenses/by/3.0/us/"),
        ("github.com/johan-socarras/Carrick", "https://github.com/johan-socarras/Carrick"),
    ] {
        again.get_by_label(label).click();
        again.step();
        let opened: Vec<String> = again
            .output()
            .platform_output
            .commands
            .iter()
            .filter_map(|c| match c {
                egui::OutputCommand::OpenUrl(open) => Some(open.url.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(opened, [url], "clicking {label}");
    }
}