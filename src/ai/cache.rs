//! Analyses kept on disk, by `AnalysisKey`, so a diff that hasn't changed
//! isn't analyzed again, even after a relaunch. They're in the platform's
//! cache directory (`~/Library/Caches/dev.docket/analyses` on macOS), and
//! the oldest are pruned past `MAX_ENTRIES`. Blocking I/O; run on the
//! background executor. Tests keep nothing on disk.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context as _, Result};

use super::{AnalysisKey, DiffAnalysis};
use crate::config::APP_DIR_NAME;

const MAX_ENTRIES: usize = 500;

fn dir() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    Some(dirs::cache_dir()?.join(APP_DIR_NAME).join("analyses"))
}

/// The analysis saved for `key`, if any. One that can't be read is as
/// good as none.
pub fn load(key: &AnalysisKey) -> Option<DiffAnalysis> {
    let path = dir()?.join(format!("{}.json", key.0));
    let json = fs::read(path).ok()?;
    serde_json::from_slice(&json).ok()
}

pub fn save(key: &AnalysisKey, analysis: &DiffAnalysis) -> Result<()> {
    let Some(dir) = dir() else {
        return Ok(());
    };
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(format!("{}.json", key.0));
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, serde_json::to_vec(analysis)?)
        .with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))?;
    prune(&dir);
    Ok(())
}

/// Removes the least recently written entries past `MAX_ENTRIES`.
fn prune(dir: &PathBuf) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, entry.path()))
        })
        .collect();
    if entries.len() <= MAX_ENTRIES {
        return;
    }
    entries.sort();
    for (_, path) in &entries[..entries.len() - MAX_ENTRIES] {
        fs::remove_file(path).ok();
    }
}
