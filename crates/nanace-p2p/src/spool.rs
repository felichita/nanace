//! Messages on disk: one file per message in a directory only the user can
//! read, written with a temporary file and a rename so a crash never leaves
//! half a record. A spool holds tens of messages, so there is no database.
//! Bodies are stored as text until the exchange ends and is cleaned up.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;

pub struct Spool {
    dir: PathBuf,
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Writes `bytes` to `path` readable by the user only, replacing the file
/// atomically.
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&tmp, path)
}

impl Spool {
    /// Opens (creating if needed) the spool under `dir`.
    pub fn open(dir: &Path) -> std::io::Result<Self> {
        private_dir(dir)?;
        for kind in ["out", "in"] {
            private_dir(&dir.join(kind))?;
        }
        Ok(Self { dir: dir.to_owned() })
    }

    fn path(&self, kind: &str, id: Uuid) -> PathBuf {
        self.dir.join(kind).join(format!("{id}.json"))
    }

    pub fn save<T: Serialize>(&self, kind: &str, id: Uuid, record: &T) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(record).map_err(std::io::Error::other)?;
        write_private(&self.path(kind, id), &bytes)
    }

    pub fn remove(&self, kind: &str, id: Uuid) {
        // Already gone is fine.
        let _ = fs::remove_file(self.path(kind, id));
    }

    /// Every readable record of `kind`. A file that does not parse (written
    /// by another version, or damaged) is skipped and reported by name.
    pub fn load<T: DeserializeOwned>(&self, kind: &str) -> std::io::Result<(Vec<T>, Vec<String>)> {
        let (mut records, mut skipped) = (Vec::new(), Vec::new());
        for entry in fs::read_dir(self.dir.join(kind))? {
            let path = entry?.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            match fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            {
                Some(record) => records.push(record),
                None => skipped.push(path.file_name().unwrap_or_default().to_string_lossy().into_owned()),
            }
        }
        Ok((records, skipped))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip_and_stay_private() {
        let dir = std::env::temp_dir().join(format!("nanace-spool-{}", Uuid::new_v4()));
        let spool = Spool::open(&dir).unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        spool.save("out", a, &serde_json::json!({ "n": 1 })).unwrap();
        spool.save("out", b, &serde_json::json!({ "n": 2 })).unwrap();
        spool.save("out", a, &serde_json::json!({ "n": 3 })).unwrap();
        fs::write(dir.join("out").join("broken.json"), b"{").unwrap();
        fs::write(dir.join("out").join("notes.txt"), b"x").unwrap();

        let (mut records, skipped): (Vec<serde_json::Value>, _) = spool.load("out").unwrap();
        records.sort_by_key(|r| r["n"].as_i64());
        assert_eq!(records, [serde_json::json!({ "n": 2 }), serde_json::json!({ "n": 3 })]);
        assert_eq!(skipped, ["broken.json"]);
        assert!(spool.load::<serde_json::Value>("in").unwrap().0.is_empty());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&dir.join("out")), 0o700);
            assert_eq!(mode(&dir.join("out").join(format!("{a}.json"))), 0o600);
        }
        spool.remove("out", a);
        spool.remove("out", a);
        assert_eq!(spool.load::<serde_json::Value>("out").unwrap().0.len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }
}
