// Docket's Chrome extension: opens GitHub pull requests in Docket when
// they're relevant, and leaves the rest of GitHub to the browser.
//
// Docket decides, and opens them: this extension's native messaging host is
// Docket itself (`src/chrome/native_host.rs`), which knows your docket and
// projects. Opening through it, rather than with a `docket://` link, also
// skips Chrome's "Open Docket?" prompt.

const HOST = "dev.docket";

// `github.com/{owner}/{name}/pull/{number}`, with anything after it, as
// Docket parses links (`src/github/links.rs`).
const PULL_REQUEST =
  /^https?:\/\/(?:www\.)?github\.com\/([^/?#]+)\/([^/?#]+)\/pull\/(\d+)(?:[/?#]|$)/i;

// Docket adds this query parameter to the pull requests it opens in the
// browser itself (Open on GitHub), which stay here.
const STAY_PARAM = "docket";

const GITHUB = { url: [{ hostEquals: "github.com" }, { hostEquals: "www.github.com" }] };

/** `owner/name#number` for a pull request's URL, else null. */
function pullRequestKey(url) {
  const match = PULL_REQUEST.exec(url);
  return match ? `${match[1]}/${match[2]}#${match[3]}`.toLowerCase() : null;
}

function staysInBrowser(url) {
  try {
    return new URL(url).searchParams.has(STAY_PARAM);
  } catch {
    return false;
  }
}

// The pull request each tab is on, so moving around one on GitHub (its
// Files tab, a reload) leaves it there. Kept in session storage too, since
// Chrome stops the service worker when it's idle.
const tabs = new Map();
const restored = chrome.storage.session.get("tabs").then(({ tabs: saved = {} }) => {
  for (const [tabId, key] of Object.entries(saved)) {
    if (!tabs.has(Number(tabId))) tabs.set(Number(tabId), key);
  }
});

function remember(tabId, key) {
  if (key) tabs.set(tabId, key);
  else tabs.delete(tabId);
  chrome.storage.session.set({ tabs: Object.fromEntries(tabs) });
}

async function enabled() {
  const { enabled = true } = await chrome.storage.local.get("enabled");
  return enabled;
}

// Why Docket last couldn't be reached, shown on the toolbar button.
let hostError = null;

async function updateAction() {
  const on = await enabled();
  let title = on
    ? "Docket: opening pull requests in Docket. Click to keep them here."
    : "Docket: keeping pull requests here. Click to open them in Docket.";
  let badge = on ? "" : "off";
  if (hostError) {
    title = `Docket can't be reached (${hostError}). In Docket, choose Install Chrome Extension… from the Docket menu.`;
    badge = "!";
  }
  await chrome.action.setTitle({ title });
  await chrome.action.setBadgeText({ text: badge });
  await chrome.action.setBadgeBackgroundColor({ color: hostError ? "#d1242f" : "#59636e" });
}

/** Sends `message` to Docket; its reply, or null if it can't be reached. */
async function send(message) {
  let reply = null;
  try {
    reply = await chrome.runtime.sendNativeMessage(HOST, message);
    hostError = null;
  } catch (error) {
    hostError = error?.message ?? String(error);
  }
  await updateAction();
  return reply;
}

/**
 * Checks Docket is there. Docket keeps this extension's files up to date
 * with its own version, so when they differ the files on disk are newer:
 * reload them, once per version.
 */
async function hello() {
  const reply = await send({ type: "hello" });
  const version = chrome.runtime.getManifest().version;
  if (!reply?.version || reply.version === version) return;
  const { reloadedFor } = await chrome.storage.local.get("reloadedFor");
  if (reloadedFor === reply.version) return;
  await chrome.storage.local.set({ reloadedFor: reply.version });
  chrome.runtime.reload();
}

/** Leaves a pull request Docket opened: back, or closed if it's all the tab has. */
async function leave(tabId) {
  remember(tabId, null);
  try {
    await chrome.tabs.goBack(tabId);
  } catch {
    await chrome.tabs.remove(tabId).catch(() => {});
  }
}

async function navigated(details) {
  if (details.frameId !== 0) return;
  await restored;
  const key = pullRequestKey(details.url);
  const previous = tabs.get(details.tabId) ?? null;
  remember(details.tabId, key);
  if (!key || key === previous) return;
  if (
    details.transitionType === "reload" ||
    details.transitionQualifiers?.includes("forward_back")
  ) {
    return;
  }
  if (staysInBrowser(details.url) || !(await enabled())) return;
  const reply = await send({ type: "open", url: details.url });
  if (reply?.opened) await leave(details.tabId);
}

// Page loads, and GitHub's own navigation between its pages.
chrome.webNavigation.onCommitted.addListener(navigated, GITHUB);
chrome.webNavigation.onHistoryStateUpdated.addListener(navigated, GITHUB);

chrome.tabs.onRemoved.addListener(async (tabId) => {
  await restored;
  if (tabs.has(tabId)) remember(tabId, null);
});

chrome.action.onClicked.addListener(async () => {
  await chrome.storage.local.set({ enabled: !(await enabled()) });
  await updateAction();
});

const PULL_REQUEST_PATTERNS = [
  "*://github.com/*/*/pull/*",
  "*://www.github.com/*/*/pull/*",
];

chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.removeAll(() => {
    chrome.contextMenus.create({
      id: "open-link",
      title: "Open in Docket",
      contexts: ["link"],
      targetUrlPatterns: PULL_REQUEST_PATTERNS,
    });
    chrome.contextMenus.create({
      id: "open-page",
      title: "Open in Docket",
      contexts: ["page"],
      documentUrlPatterns: PULL_REQUEST_PATTERNS,
    });
  });
  hello();
});

chrome.runtime.onStartup.addListener(hello);

// Asked for, so Docket opens it whether or not it's relevant.
chrome.contextMenus.onClicked.addListener((info) => {
  const url = info.menuItemId === "open-link" ? info.linkUrl : info.pageUrl;
  if (url) send({ type: "open", url, always: true });
});

updateAction();
