//! Where Carrick keeps what it must not forget: one record per transfer.
//!
//! A record is what lets a transfer continue after the program, or the
//! computer, restarts. It lives in the user's data folder, or in
//! `CARRICK_HOME` when that is set.

use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Send,
    Receive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Still has work to do; `carrick resume` picks it up.
    Active,
    /// Finished.
    Done,
    /// Stopped by the person.
    Cancelled,
}

/// Everything needed to continue one transfer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub role: Role,
    pub state: State,
    /// Seconds since the Unix epoch.
    pub created: u64,
    pub updated: u64,
    /// The four-word code, when the transfer has one.
    #[serde(default)]
    pub code: Option<String>,
    /// For a receiver: the code or ticket it was given.
    #[serde(default)]
    pub key: Option<String>,
    /// Hex secret key of this endpoint for this transfer.
    pub endpoint_secret: String,
    /// Hex shared authorization key.
    pub auth_key: String,
    /// Hex root hash, once known.
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub total_bytes: u64,
    #[serde(default)]
    pub files: u64,
    /// For a sender: what was sent, as absolute paths.
    #[serde(default)]
    pub sources: Vec<PathBuf>,
    /// For a receiver: the destination folder.
    #[serde(default)]
    pub dest: Option<PathBuf>,
    /// Folder of the blob store of this transfer.
    pub store: PathBuf,
    /// Relay override in force when the transfer was created.
    #[serde(default)]
    pub relay: Option<String>,
    /// For a sender: stop after this many receivers finish.
    #[serde(default)]
    pub receivers_wanted: Option<u32>,
    /// For a sender: receivers that finished, as hex endpoint ids.
    #[serde(default)]
    pub finished_receivers: Vec<String>,
    /// For a receiver: replace destination files that differ.
    #[serde(default)]
    pub overwrite: bool,
    /// For a receiver: what to do with incoming files whose place is taken
    /// by a different file. None in records from before it existed, where
    /// `overwrite` says it.
    #[serde(default)]
    pub existing: Option<Existing>,
    /// For a receiver: files already in their place in the destination, by
    /// their index in the transfer's file list. Each file goes there as soon
    /// as it is complete, so a stopped transfer leaves what had arrived.
    #[serde(default)]
    pub saved: Vec<u32>,
}

/// What to do with an incoming file whose place in the destination is taken
/// by a different file of the same name (identical ones never travel).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Existing {
    /// Ask the person once, when the list of files has arrived.
    Ask,
    /// Put the incoming file in its place.
    Replace,
    /// Leave the file that is there, and do not fetch the incoming one.
    Skip,
    /// Keep the file that is there and save the incoming one beside it.
    KeepBoth,
}

impl Record {
    /// What this receive does with files that already exist.
    pub fn existing(&self) -> Existing {
        self.existing.unwrap_or(if self.overwrite { Existing::Replace } else { Existing::KeepBoth })
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Carrick's data folder.
#[derive(Debug, Clone)]
pub struct Home {
    root: PathBuf,
}

impl Home {
    /// Opens the data folder, creating it if needed.
    pub fn open() -> Result<Self> {
        let root = match std::env::var_os("CARRICK_HOME") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => directories::ProjectDirs::from("", "", "Carrick")
                .context("this system has no per-user data folder")?
                .data_local_dir()
                .to_path_buf(),
        };
        Self::at(root)
    }

    pub fn at(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(root.join("transfers"))
            .with_context(|| format!("create {}", root.display()))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Folder holding everything of one transfer.
    pub fn transfer_dir(&self, id: &str) -> PathBuf {
        self.root.join("transfers").join(id)
    }

    fn record_path(&self, id: &str) -> PathBuf {
        self.transfer_dir(id).join("transfer.json")
    }

    /// Writes a record so that a crash leaves either the old or the new one.
    pub fn save(&self, record: &mut Record) -> Result<()> {
        record.updated = now_secs();
        let dir = self.transfer_dir(&record.id);
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let path = self.record_path(&record.id);
        let tmp = path.with_extension("json.tmp");
        let body = serde_json::to_vec_pretty(record)?;
        std::fs::write(&tmp, body).with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("replace {}", path.display()))?;
        Ok(())
    }

    pub fn load(&self, id: &str) -> Result<Option<Record>> {
        let path = self.record_path(id);
        match std::fs::read(&path) {
            Ok(bytes) => {
                let record = serde_json::from_slice(&bytes)
                    .with_context(|| format!("{} is damaged", path.display()))?;
                Ok(Some(record))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }

    /// All records, oldest first. A damaged record is skipped, not fatal.
    pub fn list(&self) -> Result<Vec<Record>> {
        let mut out = Vec::new();
        let dir = self.root.join("transfers");
        for entry in std::fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
            let entry = entry?;
            let Some(id) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            match self.load(&id) {
                Ok(Some(record)) => out.push(record),
                Ok(None) => {}
                Err(e) => tracing::warn!("skipping transfer {id}: {e:#}"),
            }
        }
        out.sort_by_key(|r| r.created);
        Ok(out)
    }

    /// Removes a transfer's record and its private folder.
    /// Forgets a transfer: its record and, for a receive left unfinished,
    /// the pieces that had not yet become whole files. Files already in
    /// their place in the destination stay. False if there is no such one.
    pub fn forget(&self, id: &str) -> Result<bool> {
        let Some(record) = self.load(id)? else { return Ok(false) };
        if record.role == Role::Receive && record.state == State::Active {
            // The partial pieces live next to the destination.
            let _ = std::fs::remove_dir_all(&record.store);
        }
        self.remove(id)?;
        Ok(true)
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        let dir = self.transfer_dir(id);
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("remove {}", dir.display())),
        }
    }
}

pub fn decode_key(hex_key: &str) -> Result<[u8; 32]> {
    let bytes = hex::decode(hex_key).context("damaged key in transfer record")?;
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("damaged key in transfer record"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(id: &str) -> Record {
        Record {
            id: id.to_string(),
            role: Role::Send,
            state: State::Active,
            created: now_secs(),
            updated: 0,
            code: Some("a-b-c-d".into()),
            key: None,
            endpoint_secret: hex::encode([1u8; 32]),
            auth_key: hex::encode([2u8; 32]),
            root: None,
            name: "x".into(),
            total_bytes: 10,
            files: 1,
            sources: vec![PathBuf::from("x")],
            dest: None,
            store: PathBuf::from("store"),
            relay: None,
            receivers_wanted: None,
            finished_receivers: vec![],
            overwrite: false,
            existing: None,
            saved: vec![],
        }
    }

    #[test]
    fn records_survive_a_reopen() {
        let dir = std::env::temp_dir().join(format!("carrick-home-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = Home::at(&dir).unwrap();
        let mut a = sample("aaaa");
        home.save(&mut a).unwrap();
        let mut b = sample("bbbb");
        b.created += 5;
        home.save(&mut b).unwrap();

        let again = Home::at(&dir).unwrap();
        let listed = again.list().unwrap();
        assert_eq!(listed.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["aaaa", "bbbb"]);
        assert_eq!(again.load("aaaa").unwrap().unwrap().code.as_deref(), Some("a-b-c-d"));
        assert!(again.load("zzzz").unwrap().is_none());

        // A damaged record does not hide the others.
        std::fs::write(dir.join("transfers").join("bbbb").join("transfer.json"), b"{nope").unwrap();
        assert_eq!(again.list().unwrap().len(), 1);

        again.remove("aaaa").unwrap();
        assert!(again.load("aaaa").unwrap().is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
