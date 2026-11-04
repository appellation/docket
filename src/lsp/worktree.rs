//! Docket's own worktree of a project, checked out at a PR's head or a
//! branch's tip, for language servers to read and for browsing a branch
//! that isn't checked out anywhere. Shells out to `git` (blocking), so run
//! on the background executor.
//!
//! There's one per project, reused across PRs and branches, so tabs on
//! different ones share it and the last to check out wins. Switching heads rewrites
//! only the files that differ, and build output a server leaves behind
//! (`target/`) carries over. It lives in Docket's cache directory as a
//! detached `git worktree`, so it creates no branches and leaves the user's
//! checkout alone, though `git worktree list` there shows it.
//!
//! A checkout has things Git doesn't track: installed packages, and files
//! its build generated. Without them, a TypeScript server types most of a
//! project as `any`, so [`link_dependencies`] symlinks them in from the
//! checkout. They're the checkout's versions, not the PR's, which is close
//! enough for hover.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context as _, Result, bail};

use crate::projects::ProjectId;
use crate::projects::git::{command, git};

/// Serializes checkouts, so switching PRs quickly doesn't run two in the
/// same worktree at once.
static CHECKOUT: Mutex<()> = Mutex::new(());

/// Checks out `head_oid` in the project's worktree, creating it if needed,
/// and returns its path. The commit must already be in `checkout`.
pub fn prepare(checkout: &Path, project: ProjectId, head_oid: &str) -> Result<PathBuf> {
    let dir = path_for(checkout, project)?;
    prepare_at(&dir, checkout, head_oid)?;
    Ok(dir)
}

fn prepare_at(dir: &Path, checkout: &Path, head_oid: &str) -> Result<()> {
    let _guard = CHECKOUT.lock().unwrap_or_else(|err| err.into_inner());
    if is_worktree_of(dir, checkout) {
        run(command_in(dir).args(["checkout", "--quiet", "--detach", "--force", head_oid]))
            .context("couldn't check out the branch in Docket's worktree")?;
        return Ok(());
    }

    // Left over from a worktree Git no longer knows about. It's inside
    // Docket's cache, so nothing of the user's is lost.
    if dir.exists() {
        fs::remove_dir_all(dir)
            .with_context(|| format!("couldn't remove a stale worktree at {}", dir.display()))?;
    }
    git(checkout, &["worktree", "prune"])?;
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)?;
    }
    run(command_in(checkout)
        .args(["worktree", "add", "--quiet", "--detach", "--force"])
        .arg(dir)
        .arg(head_oid))
    .context("couldn't create Docket's worktree for language servers")?;
    Ok(())
}

/// Symlinks into the worktree at `dir` what the checkout has and Git
/// ignores: each package's `node_modules`, and declaration files the build
/// generated beside their sources (`Button.module.css.d.ts`). Leaves
/// anything already there alone, so it's cheap to repeat after each
/// checkout. Returns how many links it made.
pub fn link_dependencies(checkout: &Path, dir: &Path) -> Result<usize> {
    let _guard = CHECKOUT.lock().unwrap_or_else(|err| err.into_inner());
    let null_separated = |output: String| -> Vec<PathBuf> {
        output
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .collect()
    };

    // Packages at the PR's head, so a package the PR moved gets its
    // dependencies at its new path if the checkout has them there.
    let manifests = null_separated(git(dir, &["ls-files", "-z", "--", "*package.json"])?);
    let mut links: Vec<PathBuf> = manifests
        .iter()
        .filter(|path| path.file_name().is_some_and(|name| name == "package.json"))
        .map(|path| path.with_file_name("node_modules"))
        .collect();
    // `--directory` reports wholly ignored directories (`node_modules/`) as
    // one entry instead of listing every file inside, and the pathspec
    // doesn't drop them, so keep only the files.
    let declarations = null_separated(git(
        checkout,
        &[
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
            "--",
            "*.d.ts",
        ],
    )?);
    links.extend(declarations.into_iter().filter(|path| {
        let path = path.to_string_lossy();
        !path.ends_with('/') && path.ends_with(".d.ts")
    }));

    let mut linked = 0;
    for path in links {
        let (from, to) = (checkout.join(&path), dir.join(&path));
        let parent_exists = to.parent().is_some_and(Path::is_dir);
        if from.exists() && parent_exists && fs::symlink_metadata(&to).is_err() {
            symlink(&from, &to)
                .with_context(|| format!("couldn't link {} into the worktree", path.display()))?;
            linked += 1;
        }
    }
    Ok(linked)
}

#[cfg(unix)]
fn symlink(from: &Path, to: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(from, to)
}

#[cfg(windows)]
fn symlink(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::os::windows::fs::symlink_dir(from, to)
    } else {
        std::os::windows::fs::symlink_file(from, to)
    }
}

/// `<cache>/dev.docket/worktrees/<project>/<checkout name>`, so servers
/// that show the workspace's folder name show the project's. Tests keep
/// theirs in the checkout's `.git`, which goes when the checkout does.
fn path_for(checkout: &Path, project: ProjectId) -> Result<PathBuf> {
    #[cfg(test)]
    let cache = checkout.join(".git").join("docket-test-cache");
    #[cfg(not(test))]
    let cache = dirs::cache_dir().context("no cache directory on this platform")?;
    let name = checkout
        .file_name()
        .context("the project's checkout has no folder name")?;
    Ok(cache
        .join("dev.docket")
        .join("worktrees")
        .join(project.0.to_string())
        .join(name))
}

fn is_worktree_of(dir: &Path, checkout: &Path) -> bool {
    if !dir.join(".git").is_file() {
        return false;
    }
    let common_dir = |dir: &Path| {
        git(
            dir,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )
        .ok()
        .and_then(|path| fs::canonicalize(path.trim()).ok())
    };
    common_dir(dir).is_some_and(|common| Some(common) == common_dir(checkout))
}

/// A `git` command that doesn't run the repository's hooks (a post-checkout
/// hook that installs dependencies, say) or download LFS objects.
fn command_in(dir: &Path) -> std::process::Command {
    let mut command = command(dir);
    command
        .args(["-c", "core.hooksPath=/dev/null"])
        .env("GIT_LFS_SKIP_SMUDGE", "1");
    command
}

fn run(command: &mut std::process::Command) -> Result<()> {
    let output = command
        .output()
        .context("couldn't run git; is it installed?")?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(repo: &Path, contents: &str) -> String {
        fs::write(repo.join("file.txt"), contents).unwrap();
        git(repo, &["add", "."]).unwrap();
        git(
            repo,
            &[
                "-c",
                "user.name=Docket",
                "-c",
                "user.email=docket@example.com",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-m",
                contents,
            ],
        )
        .unwrap();
        git(repo, &["rev-parse", "HEAD"]).unwrap().trim().to_owned()
    }

    #[test]
    fn creates_then_reuses_the_worktree() {
        let scratch = std::env::temp_dir().join(format!("docket-worktree-{}", std::process::id()));
        let repo = scratch.join("repo");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "--quiet"]).unwrap();
        let first = commit(&repo, "one");
        let second = commit(&repo, "two");
        let dir = scratch.join("worktrees/1/repo");

        prepare_at(&dir, &repo, &first).unwrap();
        assert_eq!(fs::read_to_string(dir.join("file.txt")).unwrap(), "one");
        prepare_at(&dir, &repo, &second).unwrap();
        assert_eq!(fs::read_to_string(dir.join("file.txt")).unwrap(), "two");
        // The user's checkout is untouched.
        assert_eq!(
            git(&repo, &["worktree", "list"]).unwrap().lines().count(),
            2
        );

        // A worktree Git forgot about is replaced.
        fs::remove_file(dir.join(".git")).unwrap();
        prepare_at(&dir, &repo, &first).unwrap();
        assert_eq!(fs::read_to_string(dir.join("file.txt")).unwrap(), "one");

        fs::remove_dir_all(&scratch).unwrap();
    }

    /// Links a real checkout's dependencies into its Docket worktree:
    /// `DOCKET_LINK_CHECK=<checkout>|<worktree> cargo test -- --ignored
    /// links_a_real_checkout --nocapture`.
    #[test]
    #[ignore = "writes links into a real worktree"]
    fn links_a_real_checkout() {
        let check = std::env::var("DOCKET_LINK_CHECK").expect("DOCKET_LINK_CHECK is set");
        let (checkout, dir) = check.split_once('|').unwrap();
        let start = std::time::Instant::now();
        let linked = link_dependencies(Path::new(checkout), Path::new(dir)).unwrap();
        eprintln!("linked {linked} in {:?}", start.elapsed());
    }

    #[test]
    fn links_what_the_checkout_installed_and_generated() {
        let scratch = std::env::temp_dir().join(format!("docket-links-{}", std::process::id()));
        let repo = scratch.join("repo");
        fs::create_dir_all(repo.join("app/node_modules/react")).unwrap();
        git(&repo, &["init", "--quiet"]).unwrap();
        fs::write(repo.join(".gitignore"), "node_modules\n*.css.d.ts\n").unwrap();
        fs::write(repo.join("app/package.json"), "{}").unwrap();
        fs::write(repo.join("app/Button.module.css"), "").unwrap();
        let head = commit(&repo, "one");
        fs::write(repo.join("app/Button.module.css.d.ts"), "export {};").unwrap();
        fs::write(repo.join("app/node_modules/react/index.d.ts"), "").unwrap();
        // An ignored directory Git reports with `--directory`.
        fs::create_dir_all(repo.join("app/node_modules.d")).unwrap();
        fs::write(repo.join(".gitignore"), "node_modules\n*.css.d.ts\n*.d/\n").unwrap();

        let dir = scratch.join("worktree");
        prepare_at(&dir, &repo, &head).unwrap();
        assert_eq!(link_dependencies(&repo, &dir).unwrap(), 2);
        assert!(dir.join("app/node_modules/react/index.d.ts").exists());
        assert!(dir.join("app/Button.module.css.d.ts").exists());
        // Already linked.
        assert_eq!(link_dependencies(&repo, &dir).unwrap(), 0);
        // Ignored, so checkouts leave the links be.
        assert!(git(&dir, &["status", "--porcelain"]).unwrap().is_empty());

        fs::remove_dir_all(&scratch).unwrap();
    }
}
