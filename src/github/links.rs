//! Links to github.com that Docket can show itself, rather than the browser.

use super::models::RepoRef;

/// A link to a pull request's conversation or its files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestLink {
    pub repo: RepoRef,
    pub number: u64,
    /// Whether it's to the pull request's files.
    pub files: bool,
}

impl PullRequestLink {
    /// Parses `https://github.com/{owner}/{name}/pull/{number}`, or its
    /// `/files` (or `/changes`), with any query or fragment. Its other tabs
    /// (commits, checks), a file at a commit, and the rest of GitHub are
    /// left to the browser.
    pub fn parse(url: &str) -> Option<Self> {
        let rest = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))?;
        let (host, path) = rest.split_once('/')?;
        let host = host.to_ascii_lowercase();
        if host != "github.com" && host != "www.github.com" {
            return None;
        }
        let path = path.split(['?', '#']).next().unwrap_or_default();
        let path = path.strip_suffix('/').unwrap_or(path);
        let mut segments = path.split('/');
        let (Some(owner), Some(name), Some("pull"), Some(number)) = (
            segments.next(),
            segments.next(),
            segments.next(),
            segments.next(),
        ) else {
            return None;
        };
        if owner.is_empty() || name.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let files = match (segments.next(), segments.next()) {
            (None, _) => false,
            (Some("files" | "changes"), None) => true,
            _ => return None,
        };
        Some(Self {
            repo: RepoRef {
                owner: owner.to_owned(),
                name: name.to_owned(),
            },
            number: number.parse().ok()?,
            files,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(repo: &str, number: u64, files: bool) -> Option<PullRequestLink> {
        Some(PullRequestLink {
            repo: repo.parse().unwrap(),
            number,
            files,
        })
    }

    #[test]
    fn parses_pull_request_links() {
        let parse = PullRequestLink::parse;
        assert_eq!(
            parse("https://github.com/zed-industries/zed/pull/42"),
            link("zed-industries/zed", 42, false)
        );
        assert_eq!(
            parse("https://github.com/acme/app/pull/7/"),
            link("acme/app", 7, false)
        );
        assert_eq!(
            parse("http://www.GitHub.com/acme/app/pull/7#issuecomment-1"),
            link("acme/app", 7, false)
        );
        assert_eq!(
            parse("https://github.com/acme/app/pull/7/files?w=1#diff-abc"),
            link("acme/app", 7, true)
        );
        assert_eq!(
            parse("https://github.com/acme/app/pull/7/changes"),
            link("acme/app", 7, true)
        );
    }

    #[test]
    fn leaves_other_links_to_the_browser() {
        for url in [
            "https://github.com/acme/app",
            "https://github.com/acme/app/issues/7",
            "https://github.com/acme/app/pull/7/commits",
            "https://github.com/acme/app/pull/7/commits/abc123",
            "https://github.com/acme/app/pull/7/files/abc123",
            "https://github.com/acme/app/pull/+7",
            "https://github.com/acme/app/pull/",
            "https://github.com/acme/app/pulls",
            "https://gist.github.com/acme/app/pull/7",
            "https://example.com/acme/app/pull/7",
            "mailto:octocat@github.com",
        ] {
            assert_eq!(PullRequestLink::parse(url), None, "{url}");
        }
    }
}
