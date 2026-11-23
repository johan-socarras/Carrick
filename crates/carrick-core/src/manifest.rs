//! What a transfer contains: the first child of its hash sequence.
//!
//! A transfer is an iroh-blobs hash sequence. Child 0 is this manifest, as
//! JSON; child `i + 1` is the content of `files[i]`. The root hash therefore
//! pins every name, size and byte of the transfer.

use std::path::{Component, Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};

pub const MANIFEST_FORMAT: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub format: u32,
    /// Short name shown to people: the file or folder that was sent.
    pub name: String,
    pub files: Vec<FileEntry>,
    /// Folders with nothing inside, which no file path would recreate.
    #[serde(default)]
    pub empty_dirs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileEntry {
    /// Path relative to the destination, with `/` between components.
    pub path: String,
    pub size: u64,
    /// Modification time, seconds since the Unix epoch.
    #[serde(default)]
    pub mtime: Option<i64>,
    /// Unix permission bits, when the sender has them.
    #[serde(default)]
    pub mode: Option<u32>,
}

impl Manifest {
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("manifest serializes")
    }

    /// Parses a manifest received from the network and checks every path.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let manifest: Manifest =
            serde_json::from_slice(bytes).context("the transfer's file list is not readable")?;
        ensure!(
            manifest.format == MANIFEST_FORMAT,
            "this transfer was made by a newer Carrick (format {}); update to receive it",
            manifest.format
        );
        let mut seen = std::collections::HashSet::new();
        for file in &manifest.files {
            check_relative(&file.path)?;
            ensure!(
                seen.insert(file.path.to_lowercase()),
                "the transfer lists `{}` twice",
                file.path
            );
        }
        for dir in &manifest.empty_dirs {
            check_relative(dir)?;
        }
        Ok(manifest)
    }
}

/// Rejects any path that could land outside the destination folder.
pub fn check_relative(path: &str) -> Result<()> {
    ensure!(!path.is_empty(), "the transfer lists an empty path");
    ensure!(!path.contains('\\'), "the transfer lists an unsafe path: {path:?}");
    for part in path.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            bail!("the transfer lists an unsafe path: {path:?}");
        }
        if part.contains(':') || part.contains('\0') {
            bail!("the transfer lists an unsafe path: {path:?}");
        }
    }
    Ok(())
}

/// Characters Windows does not allow in a file name.
const FORBIDDEN: &[char] = &['<', '>', '"', '|', '?', '*'];

/// Names Windows keeps for devices, with or without an extension: a file
/// called `CON` or `nul.txt` cannot be created there.
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "COM¹",
    "COM²", "COM³", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "LPT¹", "LPT²", "LPT³",
];

/// One path component as Windows can store it: forbidden characters become
/// `_`, trailing dots and spaces (which Windows drops) and device names get
/// a `_`, so the name keeps working and stays distinct.
pub fn windows_safe(part: &str) -> String {
    let cleaned: String = part
        .chars()
        .map(|c| if FORBIDDEN.contains(&c) || (c as u32) < 32 { '_' } else { c })
        .collect();
    let trimmed = cleaned.trim_end_matches(['.', ' ']);
    let mut name = if trimmed.len() == cleaned.len() { cleaned.clone() } else { format!("{trimmed}_") };
    let stem_len = name.find('.').unwrap_or(name.len());
    let stem = name[..stem_len].trim_end_matches(' ');
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
        name.insert(stem_len, '_');
    }
    name
}

/// Maps a checked manifest path onto the local destination folder.
pub fn local_path(dest: &Path, path: &str) -> Result<PathBuf> {
    check_relative(path)?;
    let mut out = dest.to_path_buf();
    for part in path.split('/') {
        #[cfg(windows)]
        let part = windows_safe(part);
        out.push(&part);
    }
    Ok(out)
}

/// Turns a local relative path into a manifest path.
pub fn manifest_path(relative: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => {
                let part = part
                    .to_str()
                    .with_context(|| format!("{} has a name that is not valid text", relative.display()))?;
                parts.push(part.to_string());
            }
            other => bail!("unexpected path component {other:?} in {}", relative.display()),
        }
    }
    let path = parts.join("/");
    check_relative(&path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsafe_paths_are_rejected() {
        for bad in ["", "/etc/passwd", "../x", "a/../../x", "a//b", "a/./b", "C:/x", "a\\b", "x/..", "a\0b"] {
            assert!(check_relative(bad).is_err(), "{bad:?} should be rejected");
        }
        for good in ["a", "a/b/c.txt", "with space/ñandú.bin", ".hidden/x"] {
            assert!(check_relative(good).is_ok(), "{good:?} should pass");
        }
    }

    #[test]
    fn local_path_stays_inside_destination() {
        let dest = Path::new("dest");
        let p = local_path(dest, "photos/2026/a.jpg").unwrap();
        assert!(p.starts_with(dest));
        assert_eq!(p.components().count(), 4);
        assert!(local_path(dest, "../a").is_err());
    }

    #[test]
    fn names_windows_cannot_store_are_changed() {
        assert_eq!(windows_safe("CON"), "CON_");
        assert_eq!(windows_safe("nul.txt"), "nul_.txt");
        assert_eq!(windows_safe("Com1.tar.gz"), "Com1_.tar.gz");
        assert_eq!(windows_safe("lpt²"), "lpt²_");
        assert_eq!(windows_safe("aux ."), "aux_");
        assert_eq!(windows_safe("what?.txt"), "what_.txt");
        assert_eq!(windows_safe("ends with dot."), "ends with dot_");
        for fine in ["console.txt", "CONFIG", "COM10", "nullable", "año.txt", ".hidden"] {
            assert_eq!(windows_safe(fine), fine, "{fine} should stay as it is");
        }
    }

    #[test]
    fn manifest_round_trips_and_rejects_duplicates() {
        let m = Manifest {
            format: MANIFEST_FORMAT,
            name: "photos".into(),
            files: vec![
                FileEntry { path: "photos/a.jpg".into(), size: 3, mtime: Some(1), mode: None },
                FileEntry { path: "photos/b.jpg".into(), size: 4, mtime: None, mode: Some(0o644) },
            ],
            empty_dirs: vec!["photos/empty".into()],
        };
        assert_eq!(Manifest::from_bytes(&m.to_bytes()).unwrap(), m);
        assert_eq!(m.total_bytes(), 7);
        let mut dup = m.clone();
        dup.files[1].path = "photos/A.JPG".into();
        assert!(Manifest::from_bytes(&dup.to_bytes()).is_err());
        let mut evil = m;
        evil.files[0].path = "../../outside".into();
        assert!(Manifest::from_bytes(&evil.to_bytes()).is_err());
    }
}
