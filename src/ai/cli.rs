//! Connecting with the Anthropic CLI's login (`ant auth login`), as the
//! GitHub sign-in can use `gh`'s.
//!
//! Anthropic has no OAuth apps for other clients to register, so Docket
//! can't run an OAuth flow of its own. `ant` can: it signs in through the
//! browser and keeps a refreshable OAuth profile, and `ant auth
//! print-credentials --access-token` prints a short-lived token for it,
//! refreshing it first if it's due. Docket asks for one per request, so
//! the token is never stored. Blocking; run on the background executor.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context as _, Result, bail};

use crate::auth::AccessToken;

/// Where `ant` usually lives. Apps launched from Finder get a minimal PATH
/// that excludes Homebrew and Go's bin directory.
const FALLBACK_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"];

pub fn token() -> Result<AccessToken> {
    // Tests never read the user's real token.
    if cfg!(test) {
        bail!("the Anthropic CLI isn't available in tests");
    }
    let ant = find_ant().context(
        "the Anthropic CLI (ant) isn't installed. Install it with `brew install \
         anthropics/tap/ant`, run `ant auth login`, then try again.",
    )?;
    let output = Command::new(&ant)
        .args(["auth", "print-credentials", "--access-token"])
        // An exported key or token would win over the profile `ant auth
        // login` made, and isn't an OAuth token.
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .output()
        .with_context(|| format!("couldn't run {}", ant.display()))?;
    if !output.status.success() {
        bail!(
            "the Anthropic CLI isn't signed in. Run `ant auth login`, then try again. ({})",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let token = String::from_utf8(output.stdout)?.trim().to_owned();
    if token.is_empty() {
        bail!("the Anthropic CLI returned an empty token. Run `ant auth login`, then try again.");
    }
    Ok(AccessToken::new(token))
}

fn find_ant() -> Option<PathBuf> {
    let path_dirs = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    let go_bin = dirs::home_dir().map(|home| home.join("go/bin"));
    path_dirs
        .into_iter()
        .chain(FALLBACK_DIRS.iter().map(PathBuf::from))
        .chain(go_bin)
        .map(|dir| dir.join("ant"))
        .find(|ant| ant.is_file())
}
