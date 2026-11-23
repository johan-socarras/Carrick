//! What Carrick asks of the operating system: stay awake while a transfer
//! runs, show a notification when it ends, and start again after a reboot.
//!
//! Everything here is best effort. A system that cannot do one of these
//! things still transfers files; it just does it with less comfort.

use anyhow::Result;

/// Keeps the computer from going to sleep for as long as it is held.
/// The display may still turn off.
pub struct Awake {
    #[cfg(windows)]
    release: Option<std::sync::mpsc::Sender<()>>,
    #[cfg(not(windows))]
    child: Option<std::process::Child>,
}

impl Awake {
    /// Asks the system to stay awake. Never fails: if the system has no way
    /// to be asked, the guard simply does nothing.
    pub fn hold() -> Self {
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Power::{
                SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED,
            };
            // The request belongs to the thread that makes it, so it gets a
            // thread of its own that lives exactly as long as the guard.
            let (release, wait) = std::sync::mpsc::channel::<()>();
            let spawned = std::thread::Builder::new()
                .name("carrick-awake".into())
                .spawn(move || {
                    // SAFETY: plain Win32 call with constant flags.
                    unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) };
                    let _ = wait.recv();
                    // SAFETY: as above; clears the request.
                    unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
                });
            Self { release: spawned.ok().map(|_| release) }
        }
        #[cfg(target_os = "linux")]
        {
            let child = std::process::Command::new("systemd-inhibit")
                .args([
                    "--what=sleep:idle",
                    "--who=Carrick",
                    "--why=A transfer is in progress",
                    "--mode=block",
                    "sleep",
                    "infinity",
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .ok();
            Self { child }
        }
        #[cfg(target_os = "macos")]
        {
            let child = std::process::Command::new("caffeinate")
                .args(["-i", "-w", &std::process::id().to_string()])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .ok();
            Self { child }
        }
        #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
        {
            Self { child: None }
        }
    }

    /// True if the system accepted the request.
    pub fn is_active(&self) -> bool {
        #[cfg(windows)]
        {
            self.release.is_some()
        }
        #[cfg(not(windows))]
        {
            self.child.is_some()
        }
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            if let Some(release) = self.release.take() {
                let _ = release.send(());
            }
        }
        #[cfg(not(windows))]
        {
            if let Some(mut child) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

/// Shows a notification on this computer. Nothing leaves the machine.
pub fn notify(title: &str, body: &str) {
    let result = notify_rust::Notification::new()
        .appname("Carrick")
        .summary(title)
        .body(body)
        .show();
    if let Err(e) = result {
        tracing::debug!("notification not shown: {e}");
    }
}

/// Starting `carrick resume` when the person signs in, so that a transfer
/// survives a reboot without anyone touching anything.
pub mod autostart {
    use anyhow::{bail, Context};

    use super::Result;

    /// Turns it on for the current user. Returns the command registered.
    pub fn enable() -> Result<String> {
        let exe = std::env::current_exe().context("cannot tell where carrick is installed")?;
        let exe = exe.to_str().context("carrick is installed under a path that is not valid text")?;
        set(Some(exe))
    }

    /// Turns it off.
    pub fn disable() -> Result<()> {
        set(None).map(|_| ())
    }

    #[cfg(windows)]
    fn key() -> String {
        // Tests point this at a scratch key so they never touch real startup.
        std::env::var("CARRICK_AUTOSTART_KEY")
            .unwrap_or_else(|_| r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run".to_string())
    }

    #[cfg(windows)]
    fn reg(args: &[&str]) -> Result<std::process::Output> {
        std::process::Command::new("reg")
            .args(args)
            .output()
            .context("could not run reg.exe")
    }

    #[cfg(windows)]
    fn set(exe: Option<&str>) -> Result<String> {
        let key = key();
        match exe {
            Some(exe) => {
                let command = format!("\"{exe}\" resume");
                let out = reg(&["add", &key, "/v", "Carrick", "/t", "REG_SZ", "/d", &command, "/f"])?;
                if !out.status.success() {
                    bail!("could not register the startup entry: {}", String::from_utf8_lossy(&out.stderr).trim());
                }
                Ok(command)
            }
            None => {
                if status()?.is_some() {
                    let out = reg(&["delete", &key, "/v", "Carrick", "/f"])?;
                    if !out.status.success() {
                        bail!("could not remove the startup entry: {}", String::from_utf8_lossy(&out.stderr).trim());
                    }
                }
                Ok(String::new())
            }
        }
    }

    /// The command registered to run at sign-in, if any.
    #[cfg(windows)]
    pub fn status() -> Result<Option<String>> {
        let out = reg(&["query", &key(), "/v", "Carrick"])?;
        if !out.status.success() {
            return Ok(None);
        }
        let text = String::from_utf8_lossy(&out.stdout);
        Ok(text
            .lines()
            .find_map(|line| line.split_once("REG_SZ"))
            .map(|(_, value)| value.trim().to_string()))
    }

    #[cfg(target_os = "linux")]
    fn entry_path() -> Result<std::path::PathBuf> {
        let base = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(dir) if !dir.is_empty() => std::path::PathBuf::from(dir),
            _ => directories::BaseDirs::new()
                .context("this system has no home folder")?
                .home_dir()
                .join(".config"),
        };
        Ok(base.join("autostart").join("carrick.desktop"))
    }

    #[cfg(target_os = "linux")]
    fn set(exe: Option<&str>) -> Result<String> {
        let path = entry_path()?;
        match exe {
            Some(exe) => {
                let command = format!("\"{exe}\" resume");
                let body = format!(
                    "[Desktop Entry]\nType=Application\nName=Carrick\nComment=Continue unfinished transfers\nExec={command}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
                );
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, body).with_context(|| format!("cannot write {}", path.display()))?;
                Ok(command)
            }
            None => {
                match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => bail!("cannot remove {}: {e}", path.display()),
                }
                Ok(String::new())
            }
        }
    }

    /// The command registered to run at sign-in, if any.
    #[cfg(target_os = "linux")]
    pub fn status() -> Result<Option<String>> {
        let path = entry_path()?;
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(text
                .lines()
                .find_map(|line| line.strip_prefix("Exec="))
                .map(str::to_string)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => bail!("cannot read {}: {e}", path.display()),
        }
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    fn set(_exe: Option<&str>) -> Result<String> {
        bail!("starting at sign-in is not available on this system yet; run `carrick resume` after a reboot")
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    pub fn status() -> Result<Option<String>> {
        Ok(None)
    }
}
