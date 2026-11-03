//! Signing in with the GitHub CLI's existing login (`gh auth token`).
//!
//! Requests then act as the user with the CLI's token, which works in orgs
//! that have approved the GitHub CLI but not Docket. Blocking; run on the
//! background executor.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context as _, Result, bail};

use super::AccessToken;

/// Where `gh` usually lives. Apps launched from Finder get a minimal PATH
/// that excludes Homebrew, so `PATH` alone isn't enough.
const FALLBACK_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"];

pub fn token() -> Result<AccessToken> {
    // Tests never read the user's real token.
    if cfg!(test) {
        bail!("the GitHub CLI isn't available in tests");
    }
    let gh = find_gh().context(
        "the GitHub CLI (gh) isn't installed. Install it from https://cli.github.com, \
         run `gh auth login`, then try again.",
    )?;
    let output = Command::new(&gh)
        .args(["auth", "token", "--hostname", "github.com"])
        .output()
        .with_context(|| format!("couldn't run {}", gh.display()))?;
    if !output.status.success() {
        bail!(
            "the GitHub CLI isn't signed in to github.com. Run `gh auth login`, then try again. \
             ({})",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let token = String::from_utf8(output.stdout)?.trim().to_owned();
    if token.is_empty() {
        bail!("the GitHub CLI returned an empty token. Run `gh auth login`, then try again.");
    }
    Ok(AccessToken::new(token))
}

fn find_gh() -> Option<PathBuf> {
    let path_dirs = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    path_dirs
        .into_iter()
        .chain(FALLBACK_DIRS.iter().map(PathBuf::from))
        .map(|dir| dir.join("gh"))
        .find(|gh| gh.is_file())
}
