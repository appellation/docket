# Docket

Docket is a native desktop app (Rust + GPUI) for managing GitHub pull requests,
the ones you're reviewing and the ones you're shipping. See `README.md` for
setup, auth, code layout, and conventions; this file covers the intent behind
the project so changes stay pointed in the right direction.

## The name

A docket is a court's list of pending cases. The app is meant to be your list
of pending PRs: everything waiting on you to review, and everything of yours
that is still in flight. Product decisions should reinforce that framing. Docket
answers "what needs my attention, and what's the state of my work?" before it
answers anything else.

Use "Docket" (capitalized) in user-facing text and `docket` for identifiers
(crate name, actions namespace, `dev.docket` keychain service,
`DOCKET_CLIENT_ID`, user agent).

## Goals

Docket aims to be a fully managed PR triage app, replacing the GitHub web UI
and notification email for day-to-day PR work:

- **Triage.** Surface PRs that need action (review requested, changes
  requested on yours, CI failing, ready to merge) and make it fast to decide
  what to do next. Signal over completeness.
- **In-progress tracking.** Keep track of your own open and draft PRs: review
  state, checks, unresolved threads, and what's blocking each one.
- **Ergonomic review flow.** Keyboard-driven review that beats the web UI:
  moving between files and hunks, commenting, tracking what you've already
  viewed, and submitting reviews without context switching.
- **Editor integration.** Open a PR's files or a specific line in the user's
  editor, and check out PR branches locally.
- **LSP support.** Code intelligence (go to definition, hover, references) when
  reading diffs, so reviewers can understand changes without leaving the app.
- **Notifications.** Native, actionable notifications for events that matter
  (new review requests, reviews on your PRs, CI results, mentions). Prefer
  fewer, higher-signal notifications to mirroring GitHub's firehose.
- **Organization.** Saved searches, repo and team filters, grouping, snoozing
  and pinning, so users can shape the docket around how they work.

## Principles

- **No backend.** The app talks to GitHub directly using the user's OAuth token
  (device flow). Don't introduce a server dependency for a feature; local
  state and the GitHub API should be enough.
- **Native and fast.** Lean on GPUI for a responsive, keyboard-first UI. Never
  block the UI thread on network or disk; follow the background-executor
  pattern in `README.md`.
- **GraphQL first.** Fetch GitHub data via GraphQL where possible, requesting
  only what the view needs.

## Status

Early scaffold: sign-in, projects (local checkouts connected to their GitHub
repository, which scope the inboxes), and a sidebar of PR inboxes (review
requested, authored, involved) with a PR list, a PR detail view showing the
conversation and diff, and a browser for a project's local files. The "Not
built yet" section of `README.md` tracks the near-term gaps; the goals above
are the longer arc.
