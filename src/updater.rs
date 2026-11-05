//! Updating Docket from its GitHub releases, with no server of its own.
//!
//! A release is tagged `v<version>` and has a zipped `Docket.app` for each
//! architecture attached ([`asset_name`]), as `scripts/release.sh`
//! publishes them. Docket asks GitHub for the latest release with the
//! user's sign-in (the repository may be private), and if it's newer and
//! has a build for this Mac, [`install`] downloads it, checks it, and puts
//! it in place of the running app's bundle. The running process carries
//! on from what it already loaded; [`relaunch`] starts the new one.
//!
//! This runs only from an app bundle ([`running_bundle`]): `cargo run` has
//! nothing to replace.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, bail, ensure};
use sha2::{Digest as _, Sha256};

use crate::config::{BUNDLE_ID, RELEASES_NAME, RELEASES_OWNER};
use crate::github::{GithubClient, ReleaseAsset, RepoRef};

/// A release's version: `major.minor.patch`, as Cargo and the release's
/// tag (`v0.2.0`) give it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// The running Docket's.
    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).expect("Cargo's version is major.minor.patch")
    }

    /// Parses `1.2.3` or `v1.2.3`. Prereleases (`1.2.3-beta`) and anything
    /// else are `None`: they're never updated to.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.strip_prefix('v').unwrap_or(text);
        let mut parts = text.split('.').map(|part| {
            // `parse` would take a leading `+`.
            part.bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| part.parse().ok())
                .flatten()
        });
        let version = Self {
            major: parts.next()??,
            minor: parts.next()??,
            patch: parts.next()??,
        };
        parts.next().is_none().then_some(version)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The repository Docket's releases are published to.
pub fn releases_repo() -> RepoRef {
    RepoRef {
        owner: RELEASES_OWNER.to_owned(),
        name: RELEASES_NAME.to_owned(),
    }
}

/// The name of the release asset built for this Mac, e.g.
/// `Docket-macos-aarch64.zip`.
pub fn asset_name() -> String {
    format!("Docket-macos-{}.zip", std::env::consts::ARCH)
}

/// The app bundle Docket is running from, if it is one: `Docket.app` for
/// `Docket.app/Contents/MacOS/docket`. Never in tests, so they don't
/// update themselves.
pub fn running_bundle() -> Option<PathBuf> {
    if cfg!(test) || !cfg!(target_os = "macos") {
        return None;
    }
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app")
        .then(|| bundle.to_owned())
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Downloads `asset` (`version`'s build of Docket) and installs it in
/// place of `bundle`. It's unpacked beside `bundle`, so it's on the same
/// volume and moves into place at once, and it must be Docket at
/// `version`, signed, before it replaces anything.
pub fn install(
    client: &GithubClient,
    asset: &ReleaseAsset,
    version: Version,
    bundle: &Path,
) -> Result<()> {
    let parent = bundle
        .parent()
        .with_context(|| format!("{} has no parent directory", bundle.display()))?;
    let staging = parent.join(".docket-update");
    if staging.exists() {
        fs::remove_dir_all(&staging).with_context(|| format!("removing {}", staging.display()))?;
    }
    fs::create_dir(&staging).with_context(|| format!("creating {}", staging.display()))?;
    let result = install_from(client, asset, version, bundle, &staging);
    if let Err(err) = fs::remove_dir_all(&staging) {
        eprintln!("failed to remove {}: {err}", staging.display());
    }
    result
}

fn install_from(
    client: &GithubClient,
    asset: &ReleaseAsset,
    version: Version,
    bundle: &Path,
    staging: &Path,
) -> Result<()> {
    let zip = staging.join(&asset.name);
    download(client, asset, &zip)?;

    let unpacked = staging.join("new");
    run(Command::new("/usr/bin/ditto")
        .args(["-x", "-k"])
        .arg(&zip)
        .arg(&unpacked))
    .context("unpacking the update")?;
    let new_bundle = find_bundle(&unpacked)?;
    verify(&new_bundle, version)?;

    // Out of the way first, so it can come back if the new one can't move
    // in.
    let previous = staging.join("previous.app");
    fs::rename(bundle, &previous).with_context(|| format!("moving {} aside", bundle.display()))?;
    if let Err(err) = fs::rename(&new_bundle, bundle) {
        fs::rename(&previous, bundle).ok();
        return Err(err).with_context(|| format!("moving the update to {}", bundle.display()));
    }
    Ok(())
}

/// Downloads `asset` to `path`, checking its size and digest.
fn download(client: &GithubClient, asset: &ReleaseAsset, path: &Path) -> Result<()> {
    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut out = Hashing {
        inner: io::BufWriter::new(file),
        hasher: Sha256::new(),
        len: 0,
    };
    client.download_release_asset(asset, &mut out)?;
    out.inner.flush()?;
    ensure!(
        out.len == asset.size,
        "{} was {} bytes, not {}",
        asset.name,
        out.len,
        asset.size
    );
    if let Some(expected) = &asset.sha256 {
        let actual = hex(&out.hasher.finalize());
        ensure!(
            actual.eq_ignore_ascii_case(expected),
            "{}'s SHA-256 was {actual}, not {expected}",
            asset.name
        );
    }
    Ok(())
}

/// Hashes and counts what it writes.
struct Hashing<W> {
    inner: W,
    hasher: Sha256,
    len: u64,
}

impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.len += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// The one app bundle in `dir`.
fn find_bundle(dir: &Path) -> Result<PathBuf> {
    let mut bundles = fs::read_dir(dir)?
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "app"));
    let bundle = bundles.next().context("the update has no app in it")?;
    ensure!(
        bundles.next().is_none(),
        "the update has more than one app in it"
    );
    Ok(bundle)
}

/// Checks that `bundle` is Docket at `version`, and that its signature
/// covers what's in it.
fn verify(bundle: &Path, version: Version) -> Result<()> {
    let plist = bundle.join("Contents/Info.plist");
    let id = plist_value(&plist, "CFBundleIdentifier")?;
    ensure!(id == BUNDLE_ID, "the update is {id}, not {BUNDLE_ID}");
    let bundle_version = plist_value(&plist, "CFBundleShortVersionString")?;
    ensure!(
        bundle_version == version.to_string(),
        "the update is version {bundle_version}, not {version}"
    );
    run(Command::new("/usr/bin/codesign")
        .args(["--verify", "--strict"])
        .arg(bundle))
    .context("checking the update's signature")?;
    Ok(())
}

fn plist_value(plist: &Path, key: &str) -> Result<String> {
    let output = run(Command::new("/usr/bin/plutil")
        .args(["-extract", key, "raw", "-o", "-"])
        .arg(plist))
    .with_context(|| format!("reading {key} from the update"))?;
    Ok(output.trim().to_owned())
}

/// Runs `command`, returning its output, or its error output if it fails.
fn run(command: &mut Command) -> Result<String> {
    let output = command.stdin(Stdio::null()).output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{}", stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Opens `bundle` once this process has exited, so the new version starts
/// as the old one quits (which is the caller's to do).
pub fn relaunch(bundle: &Path) -> Result<()> {
    if cfg!(test) {
        bail!("tests can't relaunch Docket");
    }
    // `$0` is the pid and `$1` the bundle, so neither is parsed as shell.
    Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"while kill -0 "$0" 2>/dev/null; do sleep 0.1; done; exec /usr/bin/open "$1""#)
        .arg(std::process::id().to_string())
        .arg(bundle)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("relaunching Docket")?;
    Ok(())
}

#[cfg(test)]
pub mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::github::FakeGithub;

    #[test]
    fn parses_versions() {
        let v = |major, minor, patch| Version {
            major,
            minor,
            patch,
        };
        assert_eq!(Version::parse("v0.2.0"), Some(v(0, 2, 0)));
        assert_eq!(Version::parse("10.0.31"), Some(v(10, 0, 31)));
        assert_eq!(Version::parse("v1.2.3-beta.1"), None);
        assert_eq!(Version::parse("v1.2"), None);
        assert_eq!(Version::parse("v1.2.3.4"), None);
        assert_eq!(Version::parse("v1.+2.3"), None);
        assert_eq!(Version::parse("latest"), None);
        assert!(v(0, 10, 0) > v(0, 9, 9));
        assert!(v(1, 0, 0) > v(0, 99, 99));
        assert_eq!(Version::current().to_string(), env!("CARGO_PKG_VERSION"));
    }

    /// A signed Docket.app at `version` in `dir`, whose executable prints
    /// `marker`.
    pub fn fake_bundle(dir: &Path, version: &str, marker: &str) -> PathBuf {
        let bundle = dir.join("Docket.app");
        fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        fs::write(
            bundle.join("Contents/MacOS/docket"),
            format!("#!/bin/sh\necho {marker}\n"),
        )
        .unwrap();
        fs::write(
            bundle.join("Contents/Info.plist"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key>
  <string>{BUNDLE_ID}</string>
  <key>CFBundleExecutable</key>
  <string>docket</string>
  <key>CFBundleShortVersionString</key>
  <string>{version}</string>
</dict>
</plist>
"#
            ),
        )
        .unwrap();
        run(Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(&bundle))
        .unwrap();
        bundle
    }

    /// `bundle` zipped as `scripts/release.sh` zips it.
    pub fn zip_bundle(bundle: &Path) -> Vec<u8> {
        let zip = bundle.with_extension("zip");
        run(Command::new("/usr/bin/ditto")
            .args(["-c", "-k", "--keepParent"])
            .arg(bundle)
            .arg(&zip))
        .unwrap();
        fs::read(zip).unwrap()
    }

    fn marker(bundle: &Path) -> String {
        fs::read_to_string(bundle.join("Contents/MacOS/docket")).unwrap()
    }

    /// A client whose latest release has `zip` as this Mac's build, at
    /// `tag`.
    fn release(tag: &str, zip: Vec<u8>) -> (GithubClient, ReleaseAsset) {
        let github = Arc::new(FakeGithub::new());
        let repo = releases_repo();
        github.set_latest_release(repo.clone(), tag, vec![(&asset_name(), zip)]);
        let client = GithubClient::fake(github);
        let release = client.latest_release(&repo).unwrap().unwrap();
        (client, release.assets[0].clone())
    }

    #[test]
    fn installs_in_place_of_the_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let installed = fake_bundle(&dir.path().join("Applications"), "0.1.0", "old");
        let new = fake_bundle(&dir.path().join("build"), "0.2.0", "new");
        let (client, asset) = release("v0.2.0", zip_bundle(&new));

        install(
            &client,
            &asset,
            Version::parse("0.2.0").unwrap(),
            &installed,
        )
        .unwrap();

        assert!(marker(&installed).contains("new"));
        // Nothing's left beside it.
        let left: Vec<_> = fs::read_dir(installed.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, vec!["Docket.app"]);
    }

    #[test]
    fn refuses_an_update_that_isnt_what_it_says() {
        let dir = tempfile::tempdir().unwrap();
        let installed = fake_bundle(&dir.path().join("Applications"), "0.1.0", "old");
        let new = fake_bundle(&dir.path().join("build"), "0.2.0", "new");
        let zip = zip_bundle(&new);

        // Released as a version it isn't.
        let (client, asset) = release("v0.3.0", zip.clone());
        let err = install(
            &client,
            &asset,
            Version::parse("0.3.0").unwrap(),
            &installed,
        );
        assert!(format!("{:#}", err.unwrap_err()).contains("not 0.3.0"));

        // Changed in transit.
        let (client, mut asset) = release("v0.2.0", zip.clone());
        asset.sha256 = Some(hex(&Sha256::digest(b"something else")));
        let err = install(
            &client,
            &asset,
            Version::parse("0.2.0").unwrap(),
            &installed,
        );
        assert!(format!("{:#}", err.unwrap_err()).contains("SHA-256"));

        // Changed after it was signed.
        fs::write(new.join("Contents/MacOS/docket"), "#!/bin/sh\necho evil\n").unwrap();
        let (client, asset) = release("v0.2.0", zip_bundle(&new));
        let err = install(
            &client,
            &asset,
            Version::parse("0.2.0").unwrap(),
            &installed,
        );
        assert!(format!("{:#}", err.unwrap_err()).contains("signature"));

        assert!(marker(&installed).contains("old"));
        assert!(!installed.parent().unwrap().join(".docket-update").exists());
    }
}
