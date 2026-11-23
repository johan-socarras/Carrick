//! `carrick`: send files from one computer to another, wherever they are.

use std::{
    io::IsTerminal,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use console::style;
use indicatif::{HumanBytes, ProgressBar, ProgressDrawTarget, ProgressStyle};
use carrick_core::{
    events::{self, Event, EventRx, Route},
    home::{Existing, Home, Record, Role, State},
    net::{NetOptions, RelayChoice},
    recv::{self, ReceiveOptions, ReceiveOutcome},
    send::{self, SendOptions, SendOutcome},
    system::{self, Awake},
    CancellationToken,
};

/// Send files from one computer to another, wherever they are.
///
/// Direct when the network allows it, encrypted end to end, verified piece by
/// piece, and it carries on by itself after a dropped connection.
#[derive(Parser, Debug)]
#[command(
    name = "carrick",
    version,
    long_version = concat!(
        env!("CARGO_PKG_VERSION"),
        "\nCopyright (c) 2026 Johan Socarras. Licensed under the MIT licence or the Apache License 2.0, at your option.",
        "\nhttps://github.com/johan-socarras/Carrick"
    ),
    about,
    max_term_width = 100
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Relay to use: `default`, `disabled`, or the URL of your own relay.
    #[arg(long, global = true, env = "CARRICK_RELAY", value_name = "RELAY")]
    relay: Option<String>,

    /// A PEM file with extra certificate authorities to trust for the relay,
    /// when your own relay has a certificate from a private authority.
    #[arg(long, global = true, env = "CARRICK_RELAY_CA", value_name = "PEM")]
    relay_ca: Option<PathBuf>,

    /// Listen on this UDP port instead of a random one. Open (forward) it on
    /// your router once, and the other side can connect directly even when
    /// its own network is strict.
    #[arg(long, global = true, env = "CARRICK_PORT", value_name = "PORT")]
    port: Option<u16>,

    /// The address this computer is reached at from outside, usually the
    /// router's public IP (`203.0.113.7`, or `203.0.113.7:43217` when the
    /// router forwards another port). It goes into the ticket and is told to
    /// the other side. Several can be given, separated by commas.
    #[arg(long, global = true, env = "CARRICK_PUBLIC_ADDR", value_name = "IP[:PORT]")]
    public_addr: Option<String>,

    /// Worker threads to use. Kept low by default so a long transfer does not
    /// take over the computer.
    #[arg(long, global = true, env = "CARRICK_THREADS", value_name = "N")]
    threads: Option<usize>,

    /// Print plain lines instead of a progress bar.
    #[arg(long, global = true)]
    plain: bool,

    /// Do not show a system notification when a transfer ends.
    #[arg(long, global = true, env = "CARRICK_NO_NOTIFY")]
    no_notify: bool,

    /// Say more about what is happening (-v, -vv).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Offer files or folders and print the code to receive them.
    Send {
        /// Files and folders to send.
        #[arg(required = true, value_name = "PATH")]
        paths: Vec<PathBuf>,
        /// Give a long ticket instead of a four-word code. A ticket needs no
        /// rendezvous server, so it also works on a network without Internet.
        #[arg(long)]
        ticket: bool,
        /// Stop once this many receivers have everything. By default the
        /// transfer stays on offer until you stop it.
        #[arg(long, value_name = "N")]
        receivers: Option<u32>,
    },
    /// Receive what someone is sending.
    #[command(visible_alias = "recv")]
    Receive {
        /// The four-word code (with dashes or spaces) or the ticket.
        #[arg(required = true, value_name = "CODE")]
        key: Vec<String>,
        /// Folder to put the files in. Defaults to the current folder.
        #[arg(short, long, value_name = "DIR")]
        out: Option<PathBuf>,
        /// Replace files in the destination that differ from the incoming ones.
        /// Without --overwrite, --skip-existing or --keep-both, Carrick asks
        /// when it finds them, or keeps both if nobody is at the terminal.
        #[arg(long, group = "existing")]
        overwrite: bool,
        /// Leave files in the destination that differ from the incoming
        /// ones as they are, and do not fetch those.
        #[arg(long, group = "existing")]
        skip_existing: bool,
        /// Keep both: the incoming file is saved beside the old one.
        #[arg(long, group = "existing")]
        keep_both: bool,
        /// After finishing, keep passing pieces to other receivers of the
        /// same transfer until you stop it.
        #[arg(long)]
        seed: bool,
    },
    /// Continue every transfer that did not finish on this computer.
    Resume,
    /// Show the transfers this computer knows about.
    #[command(visible_alias = "ls")]
    List,
    /// Forget a transfer. Received pieces that were not yet complete are deleted.
    Cancel {
        /// Identifier shown by `carrick list`.
        id: String,
    },
    /// Run `carrick resume` by itself when you sign in, so a transfer
    /// survives a reboot: `on`, `off` or `status`.
    Autostart {
        #[arg(value_enum, default_value = "status")]
        action: AutostartAction,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum AutostartAction {
    On,
    Off,
    Status,
}

fn main() {
    let cli = Cli::parse();
    let threads = cli
        .threads
        .unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(4))
        .max(2);
    // The blob store builds its own runtime; this keeps it small too.
    if std::env::var_os("TOKIO_WORKER_THREADS").is_none() {
        std::env::set_var("TOKIO_WORKER_THREADS", threads.to_string());
    }
    // Resumed transfers read their network options from the environment.
    if let Some(ca) = &cli.relay_ca {
        std::env::set_var("CARRICK_RELAY_CA", ca);
    }
    if let Some(port) = cli.port {
        std::env::set_var("CARRICK_PORT", port.to_string());
    }
    if let Some(addrs) = &cli.public_addr {
        std::env::set_var("CARRICK_PUBLIC_ADDR", addrs);
    }
    let level = match cli.verbose {
        0 => "error",
        1 => "carrick_core=debug,warn",
        _ => "debug",
    };
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| level.into()))
        .with_writer(std::io::stderr)
        .init();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(threads)
        .enable_all()
        .build()
        .expect("start runtime");
    let code = match runtime.block_on(run(cli)) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{} {e:#}", style("error:").red().bold());
            1
        }
    };
    // Do not wait for background tasks of the network stack.
    runtime.shutdown_timeout(Duration::from_secs(2));
    std::process::exit(code);
}

async fn run(cli: Cli) -> Result<i32> {
    let home = Home::open()?;
    let mut net = NetOptions::from_env()?;
    if let Some(relay) = &cli.relay {
        net.relay = RelayChoice::parse(relay)?;
    }
    let plain = cli.plain || !std::io::stderr().is_terminal();
    let notify = !cli.no_notify;
    match cli.command {
        Command::Send { paths, ticket, receivers } => {
            let _awake = Awake::hold();
            let stop = stop_on_ctrl_c();
            let (tx, rx) = events::channel();
            let ui = tokio::spawn(show(rx, plain, notify));
            let outcome = send::send(&home, SendOptions { paths, ticket, receivers, net }, tx, stop).await;
            ui.await.ok();
            finish_send(outcome, notify)
        }
        Command::Receive { key, out, overwrite, skip_existing, keep_both, seed } => {
            let _awake = Awake::hold();
            let stop = stop_on_ctrl_c();
            let (tx, rx) = events::channel();
            let existing = if overwrite {
                Existing::Replace
            } else if skip_existing {
                Existing::Skip
            } else if keep_both || !std::io::stdin().is_terminal() {
                Existing::KeepBoth
            } else {
                Existing::Ask
            };
            let (ask, answers) = if existing == Existing::Ask {
                let (tx, rx) = recv::answers();
                (Some(tx), Some(rx))
            } else {
                (None, None)
            };
            let ui = tokio::spawn(show_labelled(rx, plain, notify, String::new(), ask));
            let dest = match out {
                Some(dir) => dir,
                None => std::env::current_dir().context("no current folder")?,
            };
            let opts = ReceiveOptions { key: key.join(" "), dest, existing, answers, seed, net };
            let outcome = recv::receive(&home, opts, tx, stop).await;
            ui.await.ok();
            finish_receive(outcome, notify)
        }
        Command::Resume => resume(&home, plain, notify).await,
        Command::List => {
            list(&home)?;
            Ok(0)
        }
        Command::Cancel { id } => {
            cancel(&home, &id)?;
            Ok(0)
        }
        Command::Autostart { action } => {
            match action {
                AutostartAction::On => {
                    let command = system::autostart::enable()?;
                    println!("On. When you sign in, this will run: {command}");
                }
                AutostartAction::Off => {
                    system::autostart::disable()?;
                    println!("Off. After a reboot, run `carrick resume` yourself.");
                }
                AutostartAction::Status => match system::autostart::status()? {
                    Some(command) => println!("On: {command}"),
                    None => println!("Off. `carrick autostart on` makes transfers survive a reboot."),
                },
            }
            Ok(0)
        }
    }
}

/// First Ctrl+C asks the transfer to stop cleanly; a second one exits.
fn stop_on_ctrl_c() -> CancellationToken {
    let token = CancellationToken::new();
    let for_task = token.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            for_task.cancel();
        }
        if tokio::signal::ctrl_c().await.is_ok() {
            std::process::exit(130);
        }
    });
    token
}

/// Shows a notification if they are wanted, and passes errors through.
fn told<T>(outcome: Result<T>, notify: bool, what: &str) -> Result<T> {
    if let (Err(e), true) = (&outcome, notify) {
        system::notify(&format!("Carrick: {what} failed"), &format!("{e:#}"));
    }
    outcome
}

fn finish_send(outcome: Result<SendOutcome>, notify: bool) -> Result<i32> {
    match told(outcome, notify, "sending")? {
        SendOutcome::Completed { receivers } => {
            eprintln!("{} {receivers} receiver(s) have everything.", style("Done:").green().bold());
            Ok(0)
        }
        SendOutcome::Stopped { receivers } => {
            eprintln!(
                "Stopped. {receivers} receiver(s) finished so far. The transfer is kept: `carrick resume` offers it again."
            );
            Ok(130)
        }
    }
}

fn finish_receive(outcome: Result<ReceiveOutcome>, notify: bool) -> Result<i32> {
    match told(outcome, notify, "receiving")? {
        ReceiveOutcome::Completed { files, bytes, dest, kept_aside } => {
            if notify {
                system::notify(
                    "Carrick: transfer received",
                    &format!("{files} file(s), {}, saved in {}", HumanBytes(bytes), dest.display()),
                );
            }
            eprintln!(
                "{} {files} file(s), {}, saved in {}",
                style("Done:").green().bold(),
                HumanBytes(bytes),
                dest.display()
            );
            if !kept_aside.is_empty() {
                eprintln!(
                    "{} file(s) already existed with different content; the incoming ones were saved as:",
                    kept_aside.len()
                );
                for (_, path) in kept_aside.iter().take(20) {
                    eprintln!("  {}", path.display());
                }
                if kept_aside.len() > 20 {
                    eprintln!("  … and {} more", kept_aside.len() - 20);
                }
            }
            Ok(0)
        }
        ReceiveOutcome::Stopped => {
            eprintln!("Stopped. What arrived is kept: `carrick resume` continues from there.");
            Ok(130)
        }
    }
}

async fn resume(home: &Home, plain: bool, notify: bool) -> Result<i32> {
    let active: Vec<Record> = home.list()?.into_iter().filter(|r| r.state == State::Active).collect();
    if active.is_empty() {
        eprintln!("Nothing to resume.");
        return Ok(0);
    }
    let _awake = Awake::hold();
    let stop = stop_on_ctrl_c();
    let mut tasks = tokio::task::JoinSet::new();
    // With several transfers at once the bars would fight; use plain lines.
    let plain = plain || active.len() > 1;
    for record in active {
        let home = home.clone();
        let stop = stop.clone();
        let label = format!("[{}] ", record.id);
        tasks.spawn(async move {
            let (tx, rx) = events::channel();
            let ui = tokio::spawn(show_labelled(rx, plain, notify, label.clone(), None));
            let code = match record.role {
                Role::Send => finish_send(send::resume_send(&home, record, tx, stop).await, notify),
                Role::Receive => finish_receive(recv::resume_receive(&home, record, None, tx, stop).await, notify),
            };
            ui.await.ok();
            match code {
                Ok(code) => code,
                Err(e) => {
                    eprintln!("{label}{} {e:#}", style("error:").red().bold());
                    1
                }
            }
        });
    }
    let mut worst = 0;
    while let Some(res) = tasks.join_next().await {
        worst = worst.max(res.unwrap_or(1));
    }
    Ok(worst)
}

fn list(home: &Home) -> Result<()> {
    let records = home.list()?;
    if records.is_empty() {
        println!("No transfers yet.");
        return Ok(());
    }
    for r in records {
        let role = match r.role {
            Role::Send => "send   ",
            Role::Receive => "receive",
        };
        let state = match r.state {
            State::Active => style("active   ").yellow(),
            State::Done => style("done     ").green(),
            State::Cancelled => style("cancelled").dim(),
        };
        let place = match r.role {
            Role::Send => r.code.clone().unwrap_or_else(|| "(ticket)".to_string()),
            Role::Receive => r.dest.as_ref().map(|d| d.display().to_string()).unwrap_or_default(),
        };
        println!(
            "{:<17} {role} {state} {:>10}  {}  {}",
            r.id,
            HumanBytes(r.total_bytes).to_string(),
            if r.name.is_empty() { "(not listed yet)" } else { &r.name },
            place
        );
    }
    Ok(())
}

fn cancel(home: &Home, id: &str) -> Result<()> {
    if !home.forget(id)? {
        bail!("no transfer called `{id}`; `carrick list` shows them");
    }
    println!("Forgot transfer {id}.");
    Ok(())
}

async fn show(rx: EventRx, plain: bool, notify: bool) {
    show_labelled(rx, plain, notify, String::new(), None).await
}

/// How long data may keep going through the relay before saying it is slow.
const RELAY_HINT_AFTER: Duration = Duration::from_secs(20);

/// Turns core events into what a person sees.
/// `ask` is where the answer goes when the receive asks what to do with
/// files that already exist; without it, nobody is asked.
async fn show_labelled(mut rx: EventRx, plain: bool, notify: bool, label: String, ask: Option<recv::AnswerTx>) {
    let bar = ProgressBar::with_draw_target(None, if plain { ProgressDrawTarget::hidden() } else { ProgressDrawTarget::stderr() });
    let bytes_style = ProgressStyle::with_template(
        "{msg:<12} [{wide_bar:.cyan/blue}] {bytes}/{total_bytes}  {binary_bytes_per_sec}  eta {eta}",
    )
    .expect("template")
    .progress_chars("=> ");
    let spinner_style = ProgressStyle::with_template("{spinner} {msg}").expect("template");
    bar.set_style(spinner_style.clone());
    let say = |bar: &ProgressBar, line: String| {
        if plain {
            eprintln!("{label}{line}");
        } else {
            bar.println(format!("{label}{line}"));
        }
    };
    let mut last_plain = Instant::now() - Duration::from_secs(60);
    let mut receivers: std::collections::BTreeMap<String, u64> = Default::default();
    let mut in_bytes_mode = false;
    let mut first_progress = true;
    let mut sending = String::new();
    // Set while data goes through the relay; cleared by a direct path.
    let mut relay_since: Option<tokio::time::Instant> = None;
    let mut relay_hinted = false;
    let mut last_data: Option<Instant> = None;
    let mut last_have = 0u64;

    loop {
        let hint_at = relay_since.filter(|_| !relay_hinted).map(|since| since + RELAY_HINT_AFTER);
        let event = tokio::select! {
            event = rx.recv() => match event {
                Some(event) => event,
                None => break,
            },
            _ = async { tokio::time::sleep_until(hint_at.unwrap()).await }, if hint_at.is_some() => {
                // Only worth saying while data is actually arriving that way;
                // a dead connection also falls back to the relay path.
                if last_data.is_none_or(|at| at.elapsed() > Duration::from_secs(5)) {
                    relay_since = Some(tokio::time::Instant::now());
                    continue;
                }
                relay_hinted = true;
                say(&bar, "Still going through the relay. The free public relays are slowed down on purpose; a direct connection, or a relay of your own (--relay), is far faster.".to_string());
                continue;
            }
        };
        match event {
            Event::Hashing { done, total } => {
                if !in_bytes_mode {
                    bar.set_style(bytes_style.clone());
                    bar.set_message("Preparing");
                    bar.set_length(total);
                    in_bytes_mode = true;
                    if plain {
                        say(&bar, format!("Preparing {} …", HumanBytes(total)));
                    }
                }
                bar.set_position(done);
            }
            Event::Ready { id, code, ticket, name, files, total } => {
                sending = name.clone();
                bar.finish_and_clear();
                in_bytes_mode = false;
                say(&bar, format!("Ready to send {} — {files} file(s), {}", style(&name).bold(), HumanBytes(total)));
                say(&bar, String::new());
                say(&bar, "On the other computer run:".to_string());
                say(&bar, String::new());
                if let Some(code) = code {
                    say(&bar, format!("    carrick receive {}", style(code).green().bold()));
                }
                if let Some(ticket) = ticket {
                    say(&bar, format!("    carrick receive {ticket}"));
                }
                say(&bar, String::new());
                say(&bar, format!("Waiting for receivers (transfer {id}). Ctrl+C stops; `carrick resume` offers it again."));
            }
            Event::ReceiverJoined { receiver } => say(&bar, format!("  > {receiver} connected")),
            Event::ReceiverRoute { receiver, route } => {
                let how = match route {
                    Route::Direct => "directly, computer to computer",
                    Route::Relay => "through the relay (still encrypted end to end)",
                };
                say(&bar, format!("  > {receiver} is reached {how}"));
            }
            Event::ReceiverProgress { receiver, have, total } => {
                let percent = if total == 0 { 100 } else { have * 100 / total };
                let previous = receivers.insert(receiver.clone(), percent);
                let step = if plain { 10 } else { 5 };
                if previous.map(|p| p / step) != Some(percent / step) {
                    say(&bar, format!("  > {receiver} has {percent}% ({} of {})", HumanBytes(have), HumanBytes(total)));
                }
            }
            Event::ReceiverDone { receiver } => {
                say(&bar, format!("  {} {receiver} has everything", style("ok").green()));
                if notify {
                    system::notify("Carrick: a receiver has everything", &format!("{sending} arrived complete at {receiver}"));
                }
            }
            Event::ReceiverLeft { receiver } => say(&bar, format!("  > {receiver} disconnected")),

            Event::Connecting { attempt } => {
                if attempt == 1 {
                    bar.set_style(spinner_style.clone());
                    bar.enable_steady_tick(Duration::from_millis(120));
                    bar.set_message("Looking for the sender…");
                    in_bytes_mode = false;
                    if plain {
                        say(&bar, "Looking for the sender…".to_string());
                    }
                }
            }
            Event::Connected { route } => {
                let how = match route {
                    Route::Direct => {
                        relay_since = None;
                        "directly, computer to computer"
                    }
                    Route::Relay => {
                        relay_since.get_or_insert_with(tokio::time::Instant::now);
                        "through the relay (still encrypted end to end)"
                    }
                };
                say(&bar, format!("Connected {how}"));
            }
            Event::Listing { name, files, total } => {
                say(&bar, format!("Receiving {} — {files} file(s), {}", style(&name).bold(), HumanBytes(total)));
            }
            Event::Checking { done, total } => {
                if !in_bytes_mode {
                    bar.set_style(bytes_style.clone());
                    bar.set_length(total);
                    in_bytes_mode = true;
                }
                bar.set_message("Checking");
                bar.set_length(total);
                bar.set_position(done);
            }
            Event::AlreadyHave { files, bytes } => {
                say(&bar, format!("{files} file(s), {}, are already here and identical: they will not travel", HumanBytes(bytes)));
            }
            Event::Progress { have, total } => {
                if have > last_have {
                    last_data = Some(Instant::now());
                }
                last_have = have;
                if !in_bytes_mode {
                    bar.set_style(bytes_style.clone());
                    in_bytes_mode = true;
                }
                if first_progress {
                    first_progress = false;
                    if have > 0 && have < total {
                        say(&bar, format!("{} of {} are already here; continuing from there", HumanBytes(have), HumanBytes(total)));
                    }
                }
                bar.disable_steady_tick();
                bar.set_message("Receiving");
                bar.set_length(total);
                bar.set_position(have);
                if plain && last_plain.elapsed() >= Duration::from_secs(2) {
                    last_plain = Instant::now();
                    let percent = if total == 0 { 100 } else { have * 100 / total };
                    say(&bar, format!("{percent}%  {} of {}", HumanBytes(have), HumanBytes(total)));
                }
            }
            Event::Waiting { reason, seconds } => {
                first_progress = true;
                // The connection is gone; the route starts over with the next one.
                relay_since = None;
                say(&bar, format!("{} {reason}. Trying again in {seconds} s…", style("Interrupted:").yellow()));
            }
            Event::Saving { done, total } => {
                if in_bytes_mode {
                    bar.set_style(spinner_style.clone());
                    bar.enable_steady_tick(Duration::from_millis(120));
                    in_bytes_mode = false;
                }
                bar.set_message(format!("Saving files {done}/{total}"));
            }
            Event::Done { .. } => {}
            Event::AlreadyExist { files, bytes, examples } => {
                let Some(ask) = &ask else { continue };
                let like = examples.join(", ");
                bar.suspend(|| {
                    eprintln!(
                        "\n{files} file(s), {}, already exist in the destination with different content (like {like}).",
                        HumanBytes(bytes)
                    );
                    eprint!("[r]eplace them, [s]kip them, or [k]eep both (the incoming ones saved beside)? ");
                });
                let answer = loop {
                    let line = tokio::task::spawn_blocking(|| {
                        let mut line = String::new();
                        std::io::stdin().read_line(&mut line).map(|_| line)
                    })
                    .await;
                    match line.ok().and_then(Result::ok).map(|l| l.trim().to_lowercase()) {
                        Some(l) if l.starts_with('r') => break Existing::Replace,
                        Some(l) if l.starts_with('s') => break Existing::Skip,
                        Some(l) if l.starts_with('k') => break Existing::KeepBoth,
                        // End of input: keep both, which loses nothing.
                        None => break Existing::KeepBoth,
                        Some(_) => eprint!("r, s or k? "),
                    }
                };
                let _ = ask.send(answer);
            }
            Event::Note(text) => say(&bar, text),
        }
    }
    bar.finish_and_clear();
}
