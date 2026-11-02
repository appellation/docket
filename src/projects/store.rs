//! Persists projects as JSON in the platform config dir. Blocking disk I/O;
//! run on the background executor.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use super::{Project, ProjectId};
use crate::config::APP_DIR_NAME;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProjectStore {
    pub projects: Vec<Project>,
    /// The project the sidebar had selected, restored on launch.
    pub selected: Option<ProjectId>,
}

impl ProjectStore {
    fn path() -> Result<PathBuf> {
        let dir = dirs::config_dir().context("no config directory on this platform")?;
        Ok(dir.join(APP_DIR_NAME).join("projects.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        let mut store: Self = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(store) => store,
                Err(err) => {
                    // Move it aside so the next save doesn't destroy it.
                    let backup = path.with_extension("json.bak");
                    fs::rename(&path, &backup).ok();
                    return Err(err).with_context(|| {
                        format!("parsing {} (moved to {})", path.display(), backup.display())
                    });
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        for project in &mut store.projects {
            project.path_missing = !project.path.is_dir();
        }
        Ok(store)
    }

    /// Writes atomically (temp file + rename) so a crash can't truncate it.
    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        let dir = path.parent().expect("store path has a parent");
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_with_repository_as_string() {
        let json = r#"{
            "projects": [
                { "id": 1, "name": "zed", "path": "/src/zed", "repository": "zed-industries/zed" }
            ],
            "selected": 1
        }"#;
        let store: ProjectStore = serde_json::from_str(json).unwrap();
        assert_eq!(store.selected, Some(ProjectId(1)));
        assert_eq!(
            store.projects[0].repository.to_string(),
            "zed-industries/zed"
        );

        let value = serde_json::to_value(&store).unwrap();
        assert_eq!(value["projects"][0]["repository"], "zed-industries/zed");
        assert!(value["projects"][0].get("path_missing").is_none());
    }
}
