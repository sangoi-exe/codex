use serde::Deserialize;
use serde::Serialize;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub(crate) struct SummaryV1 {
    pub task: String,
    #[serde(default)]
    pub decisions: Vec<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub open_questions: Vec<String>,
    #[serde(default)]
    pub todo: Vec<String>,
    #[serde(default)]
    pub files_in_scope: Vec<FileInScope>,
    #[serde(default)]
    pub symbols: Vec<Symbol>,
    #[serde(default)]
    pub env: serde_json::Value,
    #[serde(default)]
    pub assumptions: Vec<String>,
    #[serde(default)]
    pub known_failures: Vec<String>,
    #[serde(default)]
    pub last_compact_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub(crate) struct FileInScope {
    pub path: String,
    pub why: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub(crate) struct Symbol {
    pub name: String,
    pub file: String,
    pub role: String,
}

/// Persist `SummaryV1` to `<codex_home>/session.json` using an atomic write.
pub(crate) fn persist_snapshot_atomic(
    codex_home: &Path,
    snapshot: &SummaryV1,
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(codex_home)?;
    let final_path = codex_home.join("session.json");
    let tmp_path = codex_home.join("session.json.tmp");

    // Serialize to pretty JSON for readability.
    let json = serde_json::to_vec_pretty(snapshot).expect("serialize snapshot");

    // Write to a temporary file first.
    {
        let mut f = File::create(&tmp_path)?;
        f.write_all(&json)?;
        f.flush()?;
        // Ensure content is on disk.
        f.sync_all()?;
    }

    // fsync the directory before renaming (best-effort; ignore errors on non-Unix).
    if let Ok(dir) = OpenOptions::new().read(true).open(codex_home) {
        #[allow(unused_must_use)]
        {
            dir.sync_all();
        }
    }

    // Atomic rename into place.
    std::fs::rename(&tmp_path, &final_path)?;

    // fsync the parent directory again after rename (best-effort).
    if let Ok(dir) = OpenOptions::new().read(true).open(codex_home) {
        #[allow(unused_must_use)]
        {
            dir.sync_all();
        }
    }

    Ok(final_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn writes_session_json_atomically() {
        let tmp = TempDir::new().unwrap();
        let codex_home = tmp.path();

        let snapshot = SummaryV1 {
            task: "demo".into(),
            last_compact_at: "2025-09-08T12:34:56Z".into(),
            ..Default::default()
        };

        let path = persist_snapshot_atomic(codex_home, &snapshot).unwrap();
        assert!(path.exists());
        let back: SummaryV1 = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(back.task, "demo");
        assert_eq!(back.last_compact_at, "2025-09-08T12:34:56Z");
    }
}
