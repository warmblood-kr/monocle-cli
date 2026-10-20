//! `~/.monocle/craft-last-session.json` — the most recent Craft session id per
//! `<profile>/<env>/<lane>`, for `craft turn --continue`. Holds ids only (no
//! tokens, no content) and is NOT a credentials file. Best-effort: a missing or
//! corrupt file means "none", and write failures never fail a turn.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::Config;

#[derive(Serialize, Deserialize)]
struct Entry {
    session_id: String,
    updated_at: String,
}

type Map = BTreeMap<String, Entry>;

#[derive(Debug, Clone)]
pub struct LastSession {
    dir: PathBuf,
    key: String,
}

impl LastSession {
    pub fn new(profile: &str, env: &str, lane: &str) -> Self {
        Self {
            dir: Config::new().dir(),
            key: format!("{profile}/{env}/{lane}"),
        }
    }

    fn path(&self) -> PathBuf {
        self.dir.join("craft-last-session.json")
    }

    fn read(&self) -> Map {
        fs::read_to_string(self.path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn get(&self) -> Option<String> {
        self.read().remove(&self.key).map(|e| e.session_id)
    }

    pub fn set(&self, id: &str) {
        let mut m = self.read();
        m.insert(
            self.key.clone(),
            Entry {
                session_id: id.to_string(),
                updated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            },
        );
        let _ = self.write(&m);
    }

    /// Drop this key's entry (the session is gone).
    pub fn clear(&self) {
        let mut m = self.read();
        if m.remove(&self.key).is_some() {
            let _ = self.write(&m);
        }
    }

    fn write(&self, m: &Map) -> std::io::Result<()> {
        use std::io::Write;
        #[cfg(unix)]
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
        if !self.dir.exists() {
            let mut b = fs::DirBuilder::new();
            b.recursive(true);
            #[cfg(unix)]
            b.mode(0o700);
            b.create(&self.dir)?;
        }
        // An existing dir (e.g. 0755 from older versions) is tightened too.
        #[cfg(unix)]
        fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700))?;
        let tmp = self.dir.join(format!(
            "craft-last-session.json.tmp.{}",
            std::process::id()
        ));
        let mut o = fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        o.mode(0o600);
        o.open(&tmp)?
            .write_all(serde_json::to_string_pretty(m)?.as_bytes())?;
        fs::rename(&tmp, self.path())
    }
}
