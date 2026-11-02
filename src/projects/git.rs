//! Reads a local checkout's GitHub remotes. Shells out to `git` (blocking),
//! so run on the background executor.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, bail};

use crate::github::RepoRef;

/// A local Git checkout and the GitHub repositories its remotes point at.
#[derive(Clone, Debug)]
pub struct Checkout {
    /// Repository root, even if a subdirectory was chosen.
    pub root: PathBuf,
    pub remotes: Vec<GitRemote>,
}

#[derive(Clone, Debug)]
pub struct GitRemote {
    /// e.g. `origin`, `upstream`
    pub name: String,
    pub repository: RepoRef,
}

impl Checkout {
    pub fn scan(path: &Path) -> Result<Self> {
        let root = git(path, &["rev-parse", "--show-toplevel"])
            .map_err(|_| anyhow::anyhow!("{} isn't inside a Git repository", path.display()))?;
        let root = PathBuf::from(root.trim());

        // Exits 1 when there are no remotes, which isn't an error here.
        let urls =
            git(&root, &["config", "--get-regexp", r"^remote\..*\.url$"]).unwrap_or_default();
        let remotes = urls
            .lines()
            .filter_map(|line| {
                let (key, url) = line.split_once(' ')?;
                let name = key.strip_prefix("remote.")?.strip_suffix(".url")?;
                Some(GitRemote {
                    name: name.to_owned(),
                    repository: parse_github_url(url)?,
                })
            })
            .collect();
        Ok(Self { root, remotes })
    }
}

/// A `git` command in `dir` that fails rather than waiting on a credential
/// prompt nobody can answer.
pub(crate) fn command(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    command
}

/// Runs `git` in `dir` and returns its stdout.
pub(crate) fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let output = command(dir)
        .args(args)
        .output()
        .context("couldn't run git; is it installed?")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)?)
}

/// Extracts `owner/name` from a github.com remote URL in any of the forms
/// Git accepts: `git@github.com:o/n.git`, `ssh://git@github.com/o/n`,
/// `https://github.com/o/n.git`, etc. Returns `None` for other hosts.
fn parse_github_url(url: &str) -> Option<RepoRef> {
    let url = url.trim();
    let (host, path) = match url.split_once("://") {
        Some((_scheme, rest)) => rest.split_once('/')?,
        // scp-like syntax: [user@]host:path
        None => url.split_once(':')?,
    };
    let host = host.rsplit_once('@').map_or(host, |(_user, host)| host);
    let host = host.split_once(':').map_or(host, |(host, _port)| host);
    if !(host.eq_ignore_ascii_case("github.com") || host.eq_ignore_ascii_case("ssh.github.com")) {
        return None;
    }
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    path.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(url: &str) -> Option<String> {
        parse_github_url(url).map(|r| r.to_string())
    }

    #[test]
    fn parses_github_remote_forms() {
        for url in [
            "git@github.com:zed-industries/zed.git",
            "git@github.com:zed-industries/zed",
            "ssh://git@github.com/zed-industries/zed.git",
            "ssh://git@ssh.github.com:443/zed-industries/zed.git",
            "https://github.com/zed-industries/zed.git",
            "https://github.com/zed-industries/zed/",
            "https://user@github.com/zed-industries/zed",
            "git://github.com/zed-industries/zed.git",
        ] {
            assert_eq!(parse(url).as_deref(), Some("zed-industries/zed"), "{url}");
        }
    }

    #[test]
    fn scans_checkout_root_and_github_remotes() {
        let dir = std::env::temp_dir().join(format!("docket-scan-{}", std::process::id()));
        let sub = dir.join("src");
        std::fs::create_dir_all(&sub).unwrap();
        let run = |args: &[&str]| git(&dir, args).unwrap();
        run(&["init", "-q"]);
        run(&["remote", "add", "origin", "git@github.com:me/zed.git"]);
        run(&[
            "remote",
            "add",
            "upstream",
            "https://github.com/zed-industries/zed",
        ]);
        run(&["remote", "add", "mirror", "git@gitlab.com:me/zed.git"]);

        let checkout = Checkout::scan(&sub).unwrap();
        assert_eq!(
            checkout.root.canonicalize().unwrap(),
            dir.canonicalize().unwrap()
        );
        let remotes: Vec<_> = checkout
            .remotes
            .iter()
            .map(|r| format!("{} {}", r.name, r.repository))
            .collect();
        assert_eq!(remotes, ["origin me/zed", "upstream zed-industries/zed"]);

        assert!(Checkout::scan(&std::env::temp_dir()).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_other_hosts_and_paths() {
        for url in [
            "git@gitlab.com:owner/repo.git",
            "https://github.example.com/owner/repo",
            "https://github.com/owner",
            "https://github.com/owner/repo/tree/main",
            "/local/path/repo.git",
        ] {
            assert_eq!(parse(url), None, "{url}");
        }
    }
}
