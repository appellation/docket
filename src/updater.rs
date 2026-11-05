//! Updating Docket from its GitHub releases, with no server of its own.
//!
//! Each build follows a [`Channel`], chosen when it's built:
//!
//! - **Stable** follows the latest release, tagged `v<version>`, and
//!   updates when its version is higher.
//! - **Main** follows [`MAIN_TAG`], a prerelease that `scripts/publish.sh`
//!   moves to `main`'s tip, and updates whenever it's at a commit other than
//!   the build's own.
//!
//! Either has a zipped `Docket.app` for each architecture attached
//! ([`asset_name`]). Once the running app is signed with a certificate
//! (`DOCKET_SIGN_IDENTITY` in `scripts/bundle.sh`), an update must be
//! signed by the same one, so publishing one takes the key, not just the
//! repository. Docket asks GitHub for its channel's release with the
//! user's sign-in (the repository may be private), and if it's an
//! [`Update`] with a build for this Mac, [`install`] downloads it, checks
//! it, and puts it in place of the running app's bundle. The running
//! process carries on from what it already loaded; [`relaunch`] starts the
//! new one.
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
use crate::github::{GithubClient, Release, ReleaseAsset, RepoRef};

/// The tag of the release main builds follow. Not `main`, which would be
/// as ambiguous as a branch's name.
pub const MAIN_TAG: &str = "main-latest";

/// The releases a build updates from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Stable,
    Main,
}

impl Channel {
    /// This build's: `DOCKET_CHANNEL` when it was built
    /// (`scripts/bundle.sh`), or stable.
    pub fn current() -> Self {
        match option_env!("DOCKET_CHANNEL") {
            Some("main") => Self::Main,
            _ => Self::Stable,
        }
    }

    /// As `DOCKET_CHANNEL` and the bundle's `DocketChannel` name it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Main => "main",
        }
    }
}

/// The commit this build is from, if `scripts/bundle.sh` built it.
pub fn current_commit() -> Option<&'static str> {
    option_env!("DOCKET_COMMIT").filter(|commit| !commit.is_empty())
}

/// A build to update to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Update {
    /// A stable release, by its version.
    Release(Version),
    /// A build of main, by its commit.
    Main { commit: String },
}

impl Update {
    /// What `release` would update a build on `channel` from `commit` to:
    /// a stable release with a higher version, or a build of main from
    /// another commit, since [`MAIN_TAG`] only moves forward.
    pub fn from_release(release: &Release, channel: Channel, commit: Option<&str>) -> Option<Self> {
        match channel {
            Channel::Stable => {
                let version = Version::parse(&release.tag)?;
                (version > Version::current()).then_some(Self::Release(version))
            }
            Channel::Main => {
                let latest = release.commit.as_deref()?;
                (commit != Some(latest)).then(|| Self::Main {
                    commit: latest.to_owned(),
                })
            }
        }
    }
}

impl fmt::Display for Update {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Release(version) => version.fmt(f),
            Self::Main { commit } => write!(f, "main {}", short(commit)),
        }
    }
}

/// How the running build is named beside an [`Update`]: `0.1.0`, or
/// `main 1a2b3c4`.
pub fn build_name(channel: Channel, commit: Option<&str>) -> String {
    match (channel, commit) {
        (Channel::Stable, _) => Version::current().to_string(),
        (Channel::Main, Some(commit)) => format!("main {}", short(commit)),
        (Channel::Main, None) => "main".to_owned(),
    }
}

fn short(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}

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

/// Downloads `asset` (`update`'s build of Docket) and installs it in
/// place of `bundle`. It's unpacked beside `bundle`, so it's on the same
/// volume and moves into place at once, and it must be Docket's `update`,
/// signed, before it replaces anything.
pub fn install(
    client: &GithubClient,
    asset: &ReleaseAsset,
    update: &Update,
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
    let result = install_from(client, asset, update, bundle, &staging);
    if let Err(err) = fs::remove_dir_all(&staging) {
        eprintln!("failed to remove {}: {err}", staging.display());
    }
    result
}

fn install_from(
    client: &GithubClient,
    asset: &ReleaseAsset,
    update: &Update,
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
    verify(&new_bundle, update)?;
    check_signature(&new_bundle, signer(bundle)?.as_deref())?;

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

/// Checks that `bundle` is Docket's `update`, on the same channel.
fn verify(bundle: &Path, update: &Update) -> Result<()> {
    let plist = bundle.join("Contents/Info.plist");
    let id = plist_value(&plist, "CFBundleIdentifier")?;
    ensure!(id == BUNDLE_ID, "the update is {id}, not {BUNDLE_ID}");
    let channel = plist_value(&plist, "DocketChannel")?;
    match update {
        Update::Release(version) => {
            ensure!(
                channel == Channel::Stable.name(),
                "the update is a {channel} build"
            );
            let bundle_version = plist_value(&plist, "CFBundleShortVersionString")?;
            ensure!(
                bundle_version == version.to_string(),
                "the update is version {bundle_version}, not {version}"
            );
        }
        Update::Main { commit } => {
            ensure!(
                channel == Channel::Main.name(),
                "the update is a {channel} build"
            );
            let bundle_commit = plist_value(&plist, "DocketCommit")?;
            ensure!(
                bundle_commit == *commit,
                "the update is from {bundle_commit}, not {commit}"
            );
        }
    }
    Ok(())
}

/// The requirement code must meet to be signed as `bundle` is: its
/// designated requirement, e.g. `identifier "dev.docket" and anchor apple
/// generic and certificate leaf[subject.OU] = TEAMID` for a Developer ID.
/// `None` if it's signed ad hoc, whose requirement is its own hash, which
/// no other build meets.
fn signer(bundle: &Path) -> Result<Option<String>> {
    let output = run(Command::new("/usr/bin/codesign")
        .args(["--display", "--requirements", "-"])
        .arg(bundle))
    .with_context(|| format!("reading {}'s signature", bundle.display()))?;
    Ok(designated_requirement(&output))
}

/// The explicit `designated => …` line of `codesign`'s requirements. Ad
/// hoc signatures only have an implicit one, `# designated => cdhash …`.
fn designated_requirement(requirements: &str) -> Option<String> {
    requirements
        .lines()
        .find_map(|line| line.strip_prefix("designated => "))
        .map(|requirement| requirement.trim().to_owned())
}

/// Checks that `bundle`'s signature covers what's in it, and that it meets
/// `requirement` (who signed this Docket, if anyone did).
fn check_signature(bundle: &Path, requirement: Option<&str>) -> Result<()> {
    let mut command = Command::new("/usr/bin/codesign");
    command.args(["--verify", "--strict"]);
    if let Some(requirement) = requirement {
        // `=` makes it the requirement itself rather than a file of one.
        command
            .arg("--test-requirement")
            .arg(format!("={requirement}"));
    }
    run(command.arg(bundle)).with_context(|| match requirement {
        Some(requirement) => {
            format!("checking the update's signature is this Docket's ({requirement})")
        }
        None => "checking the update's signature".to_owned(),
    })?;
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

    #[test]
    fn finds_updates_by_channel() {
        let release = |tag: &str, commit: &str| Release {
            tag: tag.to_owned(),
            url: String::new(),
            commit: Some(commit.to_owned()),
            assets: Vec::new(),
        };
        let stable = |tag: &str| Update::from_release(&release(tag, "abc"), Channel::Stable, None);
        assert_eq!(
            stable("v99.0.0"),
            Some(Update::Release(Version::parse("99.0.0").unwrap()))
        );
        assert_eq!(stable(&format!("v{}", Version::current())), None);
        assert_eq!(stable("v0.0.1"), None);
        assert_eq!(stable("v99.0.0-beta"), None);

        let main =
            |commit| Update::from_release(&release(MAIN_TAG, "abcdef0123"), Channel::Main, commit);
        let update = Update::Main {
            commit: "abcdef0123".into(),
        };
        assert_eq!(main(Some("1234567890")), Some(update.clone()));
        assert_eq!(main(Some("abcdef0123")), None);
        // Built by hand, from who knows where.
        assert_eq!(main(None), Some(update.clone()));
        assert_eq!(update.to_string(), "main abcdef0");
    }

    /// What a fake bundle says it is.
    pub struct FakeBuild<'a> {
        pub channel: Channel,
        pub version: &'a str,
        pub commit: &'a str,
    }

    pub fn stable_build(version: &str) -> FakeBuild<'_> {
        FakeBuild {
            channel: Channel::Stable,
            version,
            commit: "0000000",
        }
    }

    pub fn main_build(commit: &str) -> FakeBuild<'_> {
        FakeBuild {
            channel: Channel::Main,
            version: env!("CARGO_PKG_VERSION"),
            commit,
        }
    }

    /// A signed Docket.app in `dir`, as `scripts/bundle.sh` would build
    /// `build`, whose executable prints `marker`.
    pub fn fake_bundle(dir: &Path, build: FakeBuild, marker: &str) -> PathBuf {
        let FakeBuild {
            channel,
            version,
            commit,
        } = build;
        let channel = channel.name();
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
  <key>DocketChannel</key>
  <string>{channel}</string>
  <key>DocketCommit</key>
  <string>{commit}</string>
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

    /// `bundle` zipped as `scripts/package.sh` zips it.
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

    /// A client with a release that has `zip` as this Mac's build, and
    /// that build's asset.
    fn release(zip: Vec<u8>) -> (GithubClient, ReleaseAsset) {
        let github = Arc::new(FakeGithub::new());
        let repo = releases_repo();
        github.set_release(
            repo.clone(),
            "v0.0.0",
            "0000000",
            false,
            vec![(&asset_name(), zip)],
        );
        let client = GithubClient::fake(github);
        let release = client.latest_release(&repo).unwrap().unwrap();
        (client, release.assets[0].clone())
    }

    #[test]
    fn checks_who_signed_an_update() {
        let ad_hoc = "host => identifier \"com.apple.sh\" and anchor apple\n\
                      # designated => cdhash H\"dfe513b5\"\n";
        assert_eq!(designated_requirement(ad_hoc), None);
        let developer_id = "designated => identifier \"dev.docket\" and anchor apple generic \
                            and certificate leaf[subject.OU] = ABCDE12345\n";
        assert_eq!(
            designated_requirement(developer_id).as_deref(),
            Some(
                "identifier \"dev.docket\" and anchor apple generic \
                 and certificate leaf[subject.OU] = ABCDE12345"
            )
        );

        let dir = tempfile::tempdir().unwrap();
        let bundle = fake_bundle(dir.path(), stable_build("0.2.0"), "new");
        assert_eq!(signer(&bundle).unwrap(), None);
        check_signature(&bundle, None).unwrap();
        check_signature(&bundle, Some("identifier \"dev.docket\"")).unwrap();
        // Signed ad hoc, not by Apple's certificate.
        let err = check_signature(
            &bundle,
            Some("identifier \"dev.docket\" and anchor apple generic"),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("is this Docket's"));
    }

    fn version(version: &str) -> Update {
        Update::Release(Version::parse(version).unwrap())
    }

    fn main_commit(commit: &str) -> Update {
        Update::Main {
            commit: commit.to_owned(),
        }
    }

    #[test]
    fn installs_in_place_of_the_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let installed = fake_bundle(
            &dir.path().join("Applications"),
            stable_build("0.1.0"),
            "old",
        );
        let new = fake_bundle(&dir.path().join("stable"), stable_build("0.2.0"), "stable");
        let (client, asset) = release(zip_bundle(&new));

        install(&client, &asset, &version("0.2.0"), &installed).unwrap();

        assert!(marker(&installed).contains("stable"));
        // Nothing's left beside it.
        let left: Vec<_> = fs::read_dir(installed.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, vec!["Docket.app"]);

        // A build of main, the same way.
        let new = fake_bundle(&dir.path().join("main"), main_build("abc123"), "main");
        let (client, asset) = release(zip_bundle(&new));
        install(&client, &asset, &main_commit("abc123"), &installed).unwrap();
        assert!(marker(&installed).contains("main"));
    }

    #[test]
    fn refuses_an_update_that_isnt_what_it_says() {
        let dir = tempfile::tempdir().unwrap();
        let installed = fake_bundle(
            &dir.path().join("Applications"),
            stable_build("0.1.0"),
            "old",
        );
        let new = fake_bundle(&dir.path().join("stable"), stable_build("0.2.0"), "new");
        let zip = zip_bundle(&new);
        let refused = |zip: Vec<u8>, update: Update| {
            let (client, asset) = release(zip);
            let err = install(&client, &asset, &update, &installed).unwrap_err();
            format!("{err:#}")
        };

        // Released as a version, or a channel, it isn't.
        assert!(refused(zip.clone(), version("0.3.0")).contains("not 0.3.0"));
        assert!(refused(zip.clone(), main_commit("abc123")).contains("a stable build"));
        let main = zip_bundle(&fake_bundle(
            &dir.path().join("main"),
            main_build("abc123"),
            "main",
        ));
        assert!(refused(main.clone(), version("0.2.0")).contains("a main build"));
        assert!(refused(main, main_commit("def456")).contains("not def456"));

        // Changed in transit.
        let (client, mut asset) = release(zip.clone());
        asset.sha256 = Some(hex(&Sha256::digest(b"something else")));
        let err = install(&client, &asset, &version("0.2.0"), &installed);
        assert!(format!("{:#}", err.unwrap_err()).contains("SHA-256"));

        // Changed after it was signed.
        fs::write(new.join("Contents/MacOS/docket"), "#!/bin/sh\necho evil\n").unwrap();
        assert!(refused(zip_bundle(&new), version("0.2.0")).contains("signature"));

        assert!(marker(&installed).contains("old"));
        assert!(!installed.parent().unwrap().join(".docket-update").exists());
    }
}
