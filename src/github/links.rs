//! Links to github.com that Docket can show itself, rather than the browser.
//! Other apps can send them too, as `docket://` links: the same URL with
//! `docket` for its scheme.

use std::borrow::Cow;

use super::models::RepoRef;

/// Docket's URL scheme.
const SCHEME: &str = "docket";

/// The query parameter on pull requests Docket opens in the browser
/// itself, which its Chrome extension (`assets/chrome-extension`) leaves
/// there rather than sending straight back.
const STAY_PARAM: &str = "docket";

/// `url` as Docket opens it in the browser: a pull request marked to stay
/// there ([`STAY_PARAM`]), and anything else as it is.
pub fn browser_url(url: &str) -> Cow<'_, str> {
    if PullRequestLink::parse(url).is_none() {
        return url.into();
    }
    let (base, fragment) = match url.split_once('#') {
        Some((base, fragment)) => (base, Some(fragment)),
        None => (url, None),
    };
    let separator = if base.contains('?') { '&' } else { '?' };
    let mut marked = format!("{base}{separator}{STAY_PARAM}=browser");
    if let Some(fragment) = fragment {
        marked.push('#');
        marked.push_str(fragment);
    }
    marked.into()
}

/// A link to a pull request, at one of its tabs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestLink {
    pub repo: RepoRef,
    pub number: u64,
    pub page: PullRequestPage,
}

/// The part of a pull request a link is to, by Docket's tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PullRequestPage {
    Conversation,
    Commits,
    Files,
}

impl PullRequestLink {
    /// Parses `https://github.com/{owner}/{name}/pull/{number}`, or the
    /// same with `docket://`, with any query or fragment. The rest of the
    /// path picks the tab: `/files` (or `/changes`), `/commits`, and the
    /// conversation for anything else (like `/checks`). The rest of GitHub
    /// isn't a pull request.
    pub fn parse(url: &str) -> Option<Self> {
        let (scheme, rest) = url.split_once("://")?;
        if !["https", "http", SCHEME].contains(&scheme.to_ascii_lowercase().as_str()) {
            return None;
        }
        let (host, path) = rest.split_once('/')?;
        let host = host.to_ascii_lowercase();
        if host != "github.com" && host != "www.github.com" {
            return None;
        }
        let path = path.split(['?', '#']).next().unwrap_or_default();
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
        let page = match segments.next() {
            Some("files" | "changes") => PullRequestPage::Files,
            Some("commits") => PullRequestPage::Commits,
            _ => PullRequestPage::Conversation,
        };
        Some(Self {
            repo: RepoRef {
                owner: owner.to_owned(),
                name: name.to_owned(),
            },
            number: number.parse().ok()?,
            page,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(repo: &str, number: u64, page: PullRequestPage) -> Option<PullRequestLink> {
        Some(PullRequestLink {
            repo: repo.parse().unwrap(),
            number,
            page,
        })
    }

    #[test]
    fn parses_pull_request_links() {
        use PullRequestPage::*;
        let parse = PullRequestLink::parse;
        assert_eq!(
            parse("https://github.com/zed-industries/zed/pull/42"),
            link("zed-industries/zed", 42, Conversation)
        );
        assert_eq!(
            parse("https://github.com/acme/app/pull/7/"),
            link("acme/app", 7, Conversation)
        );
        assert_eq!(
            parse("http://www.GitHub.com/acme/app/pull/7#issuecomment-1"),
            link("acme/app", 7, Conversation)
        );
        assert_eq!(
            parse("https://github.com/acme/app/pull/7/checks"),
            link("acme/app", 7, Conversation)
        );
        assert_eq!(
            parse("https://github.com/acme/app/pull/7/files?w=1#diff-abc"),
            link("acme/app", 7, Files)
        );
        assert_eq!(
            parse("https://github.com/acme/app/pull/7/changes"),
            link("acme/app", 7, Files)
        );
        assert_eq!(
            parse("https://github.com/acme/app/pull/7/commits/abc123"),
            link("acme/app", 7, Commits)
        );
        assert_eq!(
            parse("docket://github.com/acme/app/pull/7/files"),
            link("acme/app", 7, Files)
        );
    }

    #[test]
    fn marks_pull_requests_opened_in_the_browser() {
        assert_eq!(
            browser_url("https://github.com/acme/app/pull/7"),
            "https://github.com/acme/app/pull/7?docket=browser"
        );
        assert_eq!(
            browser_url("https://github.com/acme/app/pull/7/files?w=1#diff-abc"),
            "https://github.com/acme/app/pull/7/files?w=1&docket=browser#diff-abc"
        );
        assert_eq!(
            browser_url("https://github.com/acme/app/commit/abc"),
            "https://github.com/acme/app/commit/abc"
        );
    }

    #[test]
    fn leaves_the_rest_of_github_alone() {
        for url in [
            "https://github.com/acme/app",
            "https://github.com/acme/app/issues/7",
            "https://github.com/acme/app/pull/+7",
            "https://github.com/acme/app/pull/",
            "https://github.com/acme/app/pulls",
            "https://gist.github.com/acme/app/pull/7",
            "https://example.com/acme/app/pull/7",
            "ftp://github.com/acme/app/pull/7",
            "docket://acme/app/pull/7",
            "mailto:octocat@github.com",
        ] {
            assert_eq!(PullRequestLink::parse(url), None, "{url}");
        }
    }
}
