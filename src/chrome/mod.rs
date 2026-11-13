//! Docket's Chrome extension (`assets/chrome-extension`), which opens pull
//! requests from the browser in Docket, and installing it from the app.
//!
//! Chrome installs extensions on its own only from its Web Store, so the
//! extension is built into Docket: Install Chrome Extension… writes it to
//! Docket's data directory, for the user to load unpacked from there, and
//! registers Docket as its native messaging host ([`native_host`]), which
//! is how the extension asks Docket whether a pull request is relevant
//! ([`OpenLinks`]) and has it opened. Its ID is pinned by the `key` in its
//! manifest, so it's the same wherever it's loaded from, and the host
//! answers only it.
//!
//! Once installed, every launch writes the files again where they've
//! changed (an update of Docket can update the extension), and the host's
//! path, in case Docket moved.

mod links;
pub mod native_host;
pub mod socket;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde_json::json;

pub use links::OpenLinks;

use crate::config::APP_DIR_NAME;

/// The native messaging host's name, which the extension sends to.
pub const HOST_NAME: &str = "dev.docket";

/// The extension's ID: from the public key in its manifest, the first 16
/// bytes of its SHA-256 written with `a` to `p` for the hex digits.
pub const EXTENSION_ID: &str = "agcejpmkgmndgamncccbhmkonkjakicj";

const MANIFEST: &str = include_str!("../../assets/chrome-extension/manifest.json");

/// The extension's files, by path in it.
const FILES: &[(&str, &[u8])] = &[
    ("manifest.json", MANIFEST.as_bytes()),
    (
        "background.js",
        include_bytes!("../../assets/chrome-extension/background.js"),
    ),
    (
        "icons/docket-16.png",
        include_bytes!("../../assets/icon/png/docket-16.png"),
    ),
    (
        "icons/docket-32.png",
        include_bytes!("../../assets/icon/png/docket-32.png"),
    ),
    (
        "icons/docket-64.png",
        include_bytes!("../../assets/icon/png/docket-64.png"),
    ),
    (
        "icons/docket-128.png",
        include_bytes!("../../assets/icon/png/docket-128.png"),
    ),
];

/// Where Chromium browsers look for native messaging hosts, under
/// Application Support. Chrome's is written whether or not it's there;
/// the rest only where the browser is.
const BROWSERS: &[&str] = &[
    "Google/Chrome",
    "Google/Chrome Beta",
    "Google/Chrome Dev",
    "Google/Chrome Canary",
    "Chromium",
    "BraveSoftware/Brave-Browser",
    "Microsoft Edge",
    "Vivaldi",
    "Arc/User Data",
];

/// The extension's version, from its manifest.
pub fn version() -> String {
    serde_json::from_str::<serde_json::Value>(MANIFEST)
        .ok()
        .and_then(|manifest| manifest["version"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Where the extension and its host go.
#[derive(Clone, Debug)]
pub struct Install {
    /// The extension's files, which the browser loads unpacked.
    pub extension_dir: PathBuf,
    /// Application Support, under which browsers keep their native
    /// messaging hosts.
    pub support_dir: PathBuf,
    /// The Docket executable browsers start as the host.
    pub executable: PathBuf,
}

impl Install {
    /// The user's, for the Docket that's running. Never in tests, which
    /// make their own in a temp dir.
    pub fn current() -> Option<Self> {
        if cfg!(test) || !cfg!(target_os = "macos") {
            return None;
        }
        let support_dir = dirs::data_dir()?;
        Some(Self {
            extension_dir: support_dir.join(APP_DIR_NAME).join("Chrome Extension"),
            executable: std::env::current_exe().ok()?.canonicalize().ok()?,
            support_dir,
        })
    }

    /// Whether it's been installed: its files are there.
    pub fn is_installed(&self) -> bool {
        self.extension_dir.join("manifest.json").is_file()
    }

    /// Writes the extension's files, and registers the host with each
    /// browser.
    pub fn install(&self) -> Result<()> {
        self.write_files()?;
        self.register_host()
    }

    /// Once installed, writes what's changed: the extension's files, and
    /// the host's path.
    pub fn refresh(&self) -> Result<()> {
        if self.is_installed() {
            self.install()?;
        }
        Ok(())
    }

    fn write_files(&self) -> Result<()> {
        for (path, contents) in FILES {
            write_if_changed(&self.extension_dir.join(path), contents)?;
        }
        Ok(())
    }

    /// The host's manifest: Docket's executable, for the extension only.
    fn host_manifest(&self) -> Vec<u8> {
        let manifest = json!({
            "name": HOST_NAME,
            "description": "Docket",
            "path": self.executable,
            "type": "stdio",
            "allowed_origins": [format!("chrome-extension://{EXTENSION_ID}/")],
        });
        serde_json::to_vec_pretty(&manifest).expect("a JSON value serializes")
    }

    fn register_host(&self) -> Result<()> {
        let manifest = self.host_manifest();
        for (ix, browser) in BROWSERS.iter().enumerate() {
            let dir = self.support_dir.join(browser);
            if ix > 0 && !dir.is_dir() {
                continue;
            }
            let path = dir
                .join("NativeMessagingHosts")
                .join(format!("{HOST_NAME}.json"));
            write_if_changed(&path, &manifest)?;
        }
        Ok(())
    }
}

fn write_if_changed(path: &Path, contents: &[u8]) -> Result<()> {
    if fs::read(path).is_ok_and(|existing| existing == contents) {
        return Ok(());
    }
    let dir = path.parent().expect("a file has a parent");
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    fs::write(path, contents).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use sha2::{Digest as _, Sha256};

    use super::*;

    #[test]
    fn extension_id_is_from_the_manifest_key() {
        let manifest: serde_json::Value = serde_json::from_str(MANIFEST).unwrap();
        let key = base64::engine::general_purpose::STANDARD
            .decode(manifest["key"].as_str().unwrap())
            .unwrap();
        let id: String = Sha256::digest(key)[..16]
            .iter()
            .flat_map(|byte| [byte >> 4, byte & 0xf])
            .map(|nibble| (b'a' + nibble) as char)
            .collect();
        assert_eq!(id, EXTENSION_ID);
        assert_eq!(version(), manifest["version"].as_str().unwrap());
    }

    #[test]
    fn the_manifest_lists_every_file() {
        let manifest: serde_json::Value = serde_json::from_str(MANIFEST).unwrap();
        let has = |path: &str| FILES.iter().any(|(file, _)| *file == path);
        assert!(has(manifest["background"]["service_worker"]
            .as_str()
            .unwrap()));
        for icon in manifest["icons"].as_object().unwrap().values() {
            assert!(has(icon.as_str().unwrap()), "{icon}");
        }
    }

    fn install_in(dir: &Path) -> Install {
        Install {
            extension_dir: dir.join("dev.docket/Chrome Extension"),
            support_dir: dir.to_owned(),
            executable: PathBuf::from("/Applications/Docket.app/Contents/MacOS/docket"),
        }
    }

    #[test]
    fn installs_the_extension_and_its_host() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("BraveSoftware/Brave-Browser")).unwrap();
        let install = install_in(dir.path());
        assert!(!install.is_installed());
        install.refresh().unwrap();
        assert!(!install.is_installed(), "refreshing doesn't install");

        install.install().unwrap();
        assert!(install.is_installed());
        for (path, contents) in FILES {
            assert_eq!(
                &fs::read(install.extension_dir.join(path)).unwrap(),
                contents
            );
        }
        let host = |browser: &str| {
            let path = dir
                .path()
                .join(browser)
                .join("NativeMessagingHosts/dev.docket.json");
            fs::read(path)
                .ok()
                .map(|json| serde_json::from_slice::<serde_json::Value>(&json).unwrap())
        };
        let chrome = host("Google/Chrome").expect("Chrome's is always written");
        assert_eq!(chrome["name"], HOST_NAME);
        assert_eq!(
            chrome["path"],
            "/Applications/Docket.app/Contents/MacOS/docket"
        );
        assert_eq!(chrome["type"], "stdio");
        assert_eq!(
            chrome["allowed_origins"],
            json!([format!("chrome-extension://{EXTENSION_ID}/")])
        );
        assert!(host("BraveSoftware/Brave-Browser").is_some());
        assert!(host("Microsoft Edge").is_none(), "only where it's there");

        // Docket moved: a launch points the host at it again.
        let moved = Install {
            executable: PathBuf::from("/Users/me/Docket.app/Contents/MacOS/docket"),
            ..install
        };
        moved.refresh().unwrap();
        assert_eq!(
            host("Google/Chrome").unwrap()["path"],
            "/Users/me/Docket.app/Contents/MacOS/docket"
        );
    }
}
