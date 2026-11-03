//! A checkout's branches for the docket: its default branch, pinned, and
//! the local branches that have your work on them, beside your pull
//! requests. Shells out to `git` (blocking), so run on the background
//! executor.
//!
//! A branch is yours when a commit on it that isn't on the default branch
//! was authored or committed by `user.email`. Branches already merged into
//! the default branch are left out, and so are those whose upstream is
//! gone, which is what a squash-merged branch looks like once GitHub
//! deletes it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};

use super::git::{Checkout, git};
use crate::github::RepoRef;

#[derive(Clone, Debug)]
pub struct Branch {
    /// Its name, e.g. `main`.
    pub name: String,
    /// Where it's read from: `refs/heads/<name>`, or for a default branch
    /// that's only on the remote, `refs/remotes/<remote>/<name>`.
    pub reference: String,
    /// Its tip.
    pub oid: String,
    /// The name of the branch it tracks on its remote, which a pull request
    /// would be from.
    pub upstream: Option<String>,
    /// When its tip was committed.
    pub updated_at: DateTime<Utc>,
    /// Its tip's subject line.
    pub subject: String,
    /// Where it's checked out: the checkout itself, or one of its
    /// worktrees.
    pub worktree: Option<PathBuf>,
}

#[derive(Clone, Debug, Default)]
pub struct Branches {
    /// The default branch: the local one where it's checked out, else the
    /// remote's (the latest you've fetched), else the local one.
    pub default: Option<Branch>,
    /// Those with your commits on them, most recently committed first.
    pub yours: Vec<Branch>,
}

/// The checkout at `root`'s default branch, and its branches with your
/// commits on them. `repository` is the project's, whose remote says which
/// branch is the default.
pub fn read(root: &Path, repository: &RepoRef) -> Result<Branches> {
    let default = default_branch(root, repository);
    Ok(Branches {
        default: match &default {
            Some(default) => pinned(root, default)?,
            None => None,
        },
        yours: yours(root, default.as_ref())?,
    })
}

/// A branch as `git for-each-ref` lists it, with what decides whether it's
/// yours.
struct Ref {
    branch: Branch,
    author: String,
    committer: String,
    /// `[gone]` when its upstream was deleted.
    track: String,
}

/// The refs matching `patterns` (full refs or prefixes), filtered by
/// `options`.
fn list_refs(root: &Path, options: &[&str], patterns: &[&str]) -> Result<Vec<Ref>> {
    const FIELDS: &[&str] = &[
        "%(refname)",
        "%(refname:lstrip=2)",
        "%(objectname)",
        "%(committerdate:unix)",
        "%(authoremail)",
        "%(committeremail)",
        "%(upstream:lstrip=3)",
        "%(upstream:track)",
        "%(worktreepath)",
        "%(contents:subject)",
    ];
    let format = format!("--format={}", FIELDS.join("%1f"));
    let mut args = vec!["for-each-ref", &format];
    args.extend(options);
    args.extend(patterns);
    let listed = git(root, &args)?;

    Ok(listed
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\x1f').collect();
            let &[
                reference,
                name,
                oid,
                date,
                author,
                committer,
                upstream,
                track,
                worktree,
                subject,
            ] = &fields[..]
            else {
                return None;
            };
            let address = |email: &str| email.trim_matches(['<', '>']).to_lowercase();
            Some(Ref {
                branch: Branch {
                    name: name.to_owned(),
                    reference: reference.to_owned(),
                    oid: oid.to_owned(),
                    upstream: (!upstream.is_empty()).then(|| upstream.to_owned()),
                    updated_at: date
                        .parse()
                        .ok()
                        .and_then(|secs| DateTime::from_timestamp(secs, 0))
                        .unwrap_or_default(),
                    subject: subject.to_owned(),
                    worktree: (!worktree.is_empty()).then(|| PathBuf::from(worktree)),
                },
                author: address(author),
                committer: address(committer),
                track: track.to_owned(),
            })
        })
        .collect())
}

/// The default branch to pin: the local one where it's checked out, since
/// that's what you're looking at; otherwise the remote's, which is as new
/// as your last fetch; otherwise the local one.
fn pinned(root: &Path, default: &DefaultBranch) -> Result<Option<Branch>> {
    let local = format!("refs/heads/{}", default.name);
    let refs = list_refs(root, &[], &[&local, &default.reference])?;
    let find = |reference: &str| refs.iter().find(|r| r.branch.reference == reference);
    let (local, remote) = (
        find(&local),
        find(&default.reference).filter(|r| r.branch.reference != local),
    );
    let pinned = match (local, remote) {
        (Some(local), _) if local.branch.worktree.is_some() => local,
        (_, Some(remote)) => remote,
        (Some(local), None) => local,
        (None, None) => return Ok(None),
    };
    Ok(Some(Branch {
        name: default.name.clone(),
        ..pinned.branch.clone()
    }))
}

/// The local branches with your commits on them, short of `default`.
fn yours(root: &Path, default: Option<&DefaultBranch>) -> Result<Vec<Branch>> {
    // Without an identity, nothing can be told apart as yours.
    let Some(email) = git(root, &["config", "user.email"])
        .ok()
        .map(|email| email.trim().to_lowercase())
        .filter(|email| !email.is_empty())
    else {
        return Ok(Vec::new());
    };
    let no_merged = default.map(|default| format!("--no-merged={}", default.oid));
    let options: Vec<&str> = no_merged.iter().map(String::as_str).collect();
    let refs = list_refs(root, &options, &["refs/heads"])?;

    let mut tip_is_yours = HashSet::new();
    let mut branches = Vec::new();
    for r in refs {
        if r.track == "[gone]" || default.is_some_and(|d| d.name == r.branch.name) {
            continue;
        }
        if r.author == email || r.committer == email {
            tip_is_yours.insert(r.branch.oid.clone());
        }
        branches.push(r.branch);
    }

    // A tip that's yours settles it. Otherwise look for your commits
    // further down, short of the default branch; without one, there's no
    // telling where the branch starts, so the tip is all there is.
    let unsettled: Vec<&str> = branches
        .iter()
        .filter(|branch| !tip_is_yours.contains(&branch.oid))
        .map(|branch| branch.oid.as_str())
        .collect();
    let yours_below = match default {
        Some(default) if !unsettled.is_empty() => {
            commits_with_yours(root, &unsettled, &default.oid, &email)?
        }
        _ => HashSet::new(),
    };
    branches
        .retain(|branch| tip_is_yours.contains(&branch.oid) || yours_below.contains(&branch.oid));
    branches.sort_by_key(|branch| std::cmp::Reverse(branch.updated_at));
    Ok(branches)
}

struct DefaultBranch {
    /// Its name as a local branch, e.g. `main`.
    name: String,
    /// The ref it was found by, local or the remote's.
    reference: String,
    oid: String,
}

/// The default branch: as the project's remote has it, or failing that a
/// local `main` or `master`.
fn default_branch(root: &Path, repository: &RepoRef) -> Option<DefaultBranch> {
    let remote = Checkout::scan(root).ok().and_then(|checkout| {
        checkout
            .remotes
            .into_iter()
            .find(|remote| remote.repository == *repository)
            .map(|remote| remote.name)
    });
    let mut candidates = Vec::new();
    if let Some(remote) = &remote {
        if let Ok(head) = git(
            root,
            &[
                "symbolic-ref",
                "--quiet",
                &format!("refs/remotes/{remote}/HEAD"),
            ],
        ) {
            candidates.push(head.trim().to_owned());
        }
        candidates.push(format!("refs/remotes/{remote}/main"));
        candidates.push(format!("refs/remotes/{remote}/master"));
    }
    candidates.push("refs/heads/main".into());
    candidates.push("refs/heads/master".into());

    candidates.into_iter().find_map(|reference| {
        let oid = git(
            root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{reference}^{{commit}}"),
            ],
        )
        .ok()?;
        let name = match &remote {
            Some(remote) => reference.strip_prefix(&format!("refs/remotes/{remote}/")),
            None => None,
        }
        .or_else(|| reference.strip_prefix("refs/heads/"))?
        .to_owned();
        Some(DefaultBranch {
            name,
            oid: oid.trim().to_owned(),
            reference,
        })
    })
}

/// Which of `tips` have a commit by `email` on them that isn't on `base`.
fn commits_with_yours(
    root: &Path,
    tips: &[&str],
    base: &str,
    email: &str,
) -> Result<HashSet<String>> {
    let mut args = vec!["log", "--topo-order", "--format=%H%x1f%P%x1f%ae%x1f%ce"];
    args.extend(tips);
    args.extend(["--not", base]);
    let log = git(root, &args)?;

    // Topological order puts children first, so going backwards each
    // commit's parents (those not on `base`) are already decided.
    let mut has_yours: HashMap<&str, bool> = HashMap::new();
    for line in log.lines().rev() {
        let mut fields = line.split('\x1f');
        let (Some(oid), Some(parents), Some(author), Some(committer)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let yours = author.eq_ignore_ascii_case(email)
            || committer.eq_ignore_ascii_case(email)
            || parents
                .split(' ')
                .any(|parent| has_yours.get(parent).copied().unwrap_or(false));
        has_yours.insert(oid, yours);
    }
    Ok(tips
        .iter()
        .filter(|tip| has_yours.get(*tip).copied().unwrap_or(false))
        .map(|tip| (*tip).to_owned())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_unmerged_branches_with_your_commits() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let run = |args: &[&str]| git(root, args).unwrap();
        let commit = |email: &str, message: &str, date: &str| {
            std::fs::write(root.join("file.txt"), message).unwrap();
            run(&["add", "-A"]);
            let date = format!("{date}T12:00:00Z");
            let status = super::super::git::command(root)
                .args([
                    "-c",
                    &format!("user.email={email}"),
                    "-c",
                    "user.name=Someone",
                ])
                .args(["commit", "-q", "-m", message])
                .env("GIT_AUTHOR_DATE", &date)
                .env("GIT_COMMITTER_DATE", &date)
                .status()
                .unwrap();
            assert!(status.success());
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "Me@Example.com"]);
        commit("other@example.com", "Initial", "2026-01-01");

        // Merged into main: not pending.
        run(&["checkout", "-q", "-b", "merged"]);
        commit("me@example.com", "Merged work", "2026-01-02");
        run(&["checkout", "-q", "main"]);
        run(&["merge", "-q", "--ff-only", "merged"]);

        // Someone else's.
        run(&["checkout", "-q", "-b", "theirs"]);
        commit("other@example.com", "Their work", "2026-01-03");

        // Yours, with someone else's commit on top.
        run(&["checkout", "-q", "main"]);
        run(&["checkout", "-q", "-b", "shared"]);
        commit("me@example.com", "Start sharing", "2026-01-04");
        commit("other@example.com", "Help out", "2026-01-05");

        // Yours, and checked out.
        run(&["checkout", "-q", "main"]);
        run(&["checkout", "-q", "-b", "mine"]);
        commit("me@example.com", "Add sprockets", "2026-01-06");

        let Branches { default, yours } = read(root, &"acme/widgets".parse().unwrap()).unwrap();
        let default = default.expect("main is the default");
        assert_eq!(default.name, "main");
        assert_eq!(default.reference, "refs/heads/main");
        assert_eq!(default.subject, "Merged work");
        let branches = yours;
        let names: Vec<_> = branches.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["mine", "shared"]);
        assert_eq!(branches[0].subject, "Add sprockets");
        assert_eq!(
            branches[0]
                .worktree
                .as_ref()
                .map(|p| p.canonicalize().unwrap()),
            Some(root.canonicalize().unwrap())
        );
        assert_eq!(branches[1].worktree, None);
        assert_eq!(
            branches[1].updated_at.to_rfc3339(),
            "2026-01-05T12:00:00+00:00"
        );

        // The remote's default is newer than a local one that isn't
        // checked out.
        run(&["remote", "add", "origin", "git@github.com:acme/widgets.git"]);
        run(&["update-ref", "refs/remotes/origin/main", "theirs"]);
        let default = |root| {
            read(root, &"acme/widgets".parse().unwrap())
                .unwrap()
                .default
                .unwrap()
        };
        let remote = default(root);
        assert_eq!(
            (remote.name.as_str(), remote.reference.as_str()),
            ("main", "refs/remotes/origin/main")
        );
        assert_eq!(remote.subject, "Their work");

        // But where it's checked out, that's what's shown.
        run(&["checkout", "-q", "main"]);
        let local = default(root);
        assert_eq!(local.reference, "refs/heads/main");
        assert!(local.worktree.is_some());
    }
}
