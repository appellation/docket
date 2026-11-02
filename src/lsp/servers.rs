//! The language servers Docket knows, and how it finds each one for a
//! project. Blocking (it reads files and may run `rustup`), so run on the
//! background executor.
//!
//! The project's own toolchain comes first, found from the user's checkout
//! rather than Docket's worktree: the checkout has its dependencies
//! installed, and it's the user's code, not the PR's. That means the
//! project's TypeScript, its `.venv`, its Rust toolchain, and the `gopls`
//! its `go.mod` pins. Servers on `PATH` or in the usual install locations
//! come next. Docket doesn't install anything.
//!
//! To add a server, add an entry to `SERVERS` naming the highlighting
//! languages (`syntax/languages.rs`) it covers.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

use crate::syntax::Language;

pub struct ServerSpec {
    /// Shown in the Files tab's status.
    pub name: &'static str,
    /// Highlighting language names this server handles.
    languages: &'static [&'static str],
    /// Whether the server runs code from the project it analyzes (build
    /// scripts, macros, toolchain files), so it shouldn't start on a PR
    /// from a fork without the user's say-so.
    pub runs_project_code: bool,
    /// Whether it needs the checkout's installed packages and generated
    /// files linked into the worktree (see `worktree::link_dependencies`).
    pub links_dependencies: bool,
    resolve: fn(&Path) -> Result<Resolved, Missing>,
}

/// How to run a server for a project.
#[derive(Clone, Debug)]
pub struct Resolved {
    pub binary: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub initialization_options: Value,
    /// Answers to `workspace/configuration`, by section (`"python"`).
    pub configuration: Value,
    /// Where it came from, for the status line: "project TypeScript 6.0.2".
    pub source: String,
}

/// A server that isn't installed, and the command that installs it.
#[derive(Clone, Debug)]
pub struct Missing {
    pub install: String,
}

const MACOS: bool = cfg!(target_os = "macos");

static SERVERS: &[ServerSpec] = &[
    ServerSpec {
        name: "rust-analyzer",
        languages: &["Rust"],
        runs_project_code: true,
        links_dependencies: false,
        resolve: rust_analyzer,
    },
    ServerSpec {
        name: "typescript-language-server",
        languages: &["TypeScript", "TSX", "JavaScript"],
        runs_project_code: false,
        links_dependencies: true,
        resolve: typescript,
    },
    ServerSpec {
        name: "gopls",
        languages: &["Go"],
        runs_project_code: true,
        links_dependencies: false,
        resolve: gopls,
    },
    ServerSpec {
        name: "basedpyright",
        languages: &["Python"],
        runs_project_code: false,
        links_dependencies: false,
        resolve: python,
    },
];

/// The server for files in `language`, if Docket knows one.
pub fn server_for(language: &Language) -> Option<&'static ServerSpec> {
    SERVERS
        .iter()
        .find(|spec| spec.languages.contains(&language.name))
}

impl ServerSpec {
    /// How to run this server for the project checked out at `checkout`.
    pub fn resolve(&self, checkout: &Path) -> Result<Resolved, Missing> {
        (self.resolve)(checkout)
    }
}

/// The LSP `languageId` for a document.
pub fn language_id(path: &str, language: &Language) -> &'static str {
    let jsx = path.ends_with(".jsx");
    match language.name {
        "Rust" => "rust",
        "TypeScript" => "typescript",
        "TSX" => "typescriptreact",
        "JavaScript" if jsx => "javascriptreact",
        "JavaScript" => "javascript",
        "Python" => "python",
        "Go" => "go",
        _ => "plaintext",
    }
}

impl Resolved {
    fn new(binary: PathBuf, source: impl Into<String>) -> Self {
        Self {
            binary,
            args: Vec::new(),
            env: Vec::new(),
            initialization_options: Value::Null,
            configuration: Value::Null,
            source: source.into(),
        }
    }

    fn args(mut self, args: &[&str]) -> Self {
        self.args = args.iter().map(|arg| (*arg).to_owned()).collect();
        self
    }
}

/// rust-analyzer from the toolchain the checkout uses (its
/// `rust-toolchain.toml`), pinned with `RUSTUP_TOOLCHAIN` so the `cargo`
/// and `rustc` it runs in the worktree use that toolchain too, not one the
/// PR names.
fn rust_analyzer(checkout: &Path) -> Result<Resolved, Missing> {
    let options = json!({"checkOnSave": false, "diagnostics": {"enable": false}});
    let with_options = |mut resolved: Resolved| {
        resolved.initialization_options = options.clone();
        resolved.configuration = json!({"rust-analyzer": options.clone()});
        resolved
    };
    let Some(rustup) = find("rustup") else {
        return find("rust-analyzer")
            .map(|binary| with_options(Resolved::new(binary, "system")))
            .ok_or_else(|| Missing {
                install: "rustup component add rust-analyzer".into(),
            });
    };
    let run = |args: &[&str]| {
        let output = Command::new(&rustup)
            .args(args)
            .current_dir(checkout)
            .env("PATH", search_path())
            .stdin(Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())?;
        Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    // "stable-aarch64-apple-darwin (default)"
    let toolchain = run(&["show", "active-toolchain"])
        .and_then(|line| line.split_whitespace().next().map(str::to_owned));
    let missing = || Missing {
        install: match &toolchain {
            Some(toolchain) => {
                format!("rustup component add rust-analyzer --toolchain {toolchain}")
            }
            None => "rustup component add rust-analyzer".into(),
        },
    };
    let binary = run(&["which", "rust-analyzer"]).ok_or_else(missing)?;
    let toolchain = toolchain.clone().ok_or_else(missing)?;
    let mut resolved = with_options(Resolved::new(
        PathBuf::from(binary),
        format!("project toolchain {toolchain}"),
    ));
    resolved.env = vec![("RUSTUP_TOOLCHAIN".into(), toolchain)];
    Ok(resolved)
}

/// typescript-language-server, given the project's own TypeScript. The
/// worktree's `node_modules` are links to the checkout's, so without
/// `tsserver.path` the server would find whatever TypeScript `typescript`
/// resolves to there, which pnpm aliases can leave without a `tsserver.js`.
/// Large projects need more memory than tsserver's default, so the limit
/// the project sets for VS Code applies too.
fn typescript(checkout: &Path) -> Result<Resolved, Missing> {
    let local = checkout.join("node_modules/.bin/typescript-language-server");
    let (binary, server) = if local.is_file() {
        (local, "project server")
    } else {
        let binary = find("typescript-language-server").ok_or_else(|| Missing {
            install: if MACOS {
                "brew install typescript-language-server".into()
            } else {
                "npm install -g typescript-language-server typescript".into()
            },
        })?;
        (binary, "system server")
    };
    let mut resolved = Resolved::new(binary, server).args(&["--stdio"]);
    let mut options = serde_json::Map::new();
    if let Some((tsserver, version)) = project_tsserver(checkout) {
        options.insert("tsserver".into(), json!({"path": tsserver}));
        resolved.source = format!("project TypeScript {version}");
    }
    if let Some(megabytes) = vscode_setting(checkout, "typescript.tsserver.maxTsServerMemory")
        .and_then(|value| value.as_u64())
    {
        options.insert("maxTsServerMemory".into(), megabytes.into());
    }
    resolved.initialization_options = Value::Object(options);
    Ok(resolved)
}

/// A setting from the checkout's `.vscode/settings.json`, where projects
/// tune their editor tooling.
fn vscode_setting(checkout: &Path, key: &str) -> Option<Value> {
    let settings = fs::read_to_string(checkout.join(".vscode/settings.json")).ok()?;
    let settings: Value = serde_json::from_str(&strip_jsonc(&settings)).ok()?;
    settings.get(key).cloned()
}

/// JSON with comments (as VS Code writes settings) to plain JSON: drops
/// `//` and `/* */` comments outside strings, and commas before `}` or `]`.
fn strip_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => out.extend(chars.next()),
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_string = true;
                out.push(c);
            }
            ('/', Some('/')) => while chars.next_if(|&c| c != '\n').is_some() {},
            ('/', Some('*')) => {
                chars.next();
                while let Some(c) = chars.next() {
                    if c == '*' && chars.next_if_eq(&'/').is_some() {
                        break;
                    }
                }
            }
            _ => out.push(c),
        }
    }
    // Trailing commas, now that comments are gone.
    let mut cleaned = String::with_capacity(out.len());
    let mut in_string = false;
    let mut escaped = false;
    for (ix, c) in out.char_indices() {
        if in_string {
            in_string = escaped || c != '"';
            escaped = !escaped && c == '\\';
        } else if c == '"' {
            in_string = true;
        } else if c == ',' && out[ix + 1..].trim_start().starts_with(['}', ']']) {
            continue;
        }
        cleaned.push(c);
    }
    cleaned
}

/// The checkout's `typescript/lib/tsserver.js` and its version. Follows
/// pnpm's aliases: `typescript: npm:@typescript/typescript6@6.0.2` installs
/// a package without `tsserver.js`, with the real one in pnpm's store.
fn project_tsserver(checkout: &Path) -> Option<(PathBuf, String)> {
    let package = checkout.join("node_modules/typescript");
    let manifest: Value =
        serde_json::from_slice(&fs::read(package.join("package.json")).ok()?).ok()?;
    let version = manifest["version"].as_str()?.to_owned();
    [
        package.join("lib/tsserver.js"),
        checkout.join(format!(
            "node_modules/.pnpm/typescript@{version}/node_modules/typescript/lib/tsserver.js"
        )),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .and_then(|path| fs::canonicalize(path).ok())
    .map(|path| (path, version))
}

/// The `gopls` the project's `go.mod` declares as a tool, or else one on
/// the system.
fn gopls(checkout: &Path) -> Result<Resolved, Missing> {
    let declares_tool = fs::read_to_string(checkout.join("go.mod"))
        .is_ok_and(|go_mod| declares_go_tool(&go_mod, "golang.org/x/tools/gopls"));
    if declares_tool && let Some(go) = find("go") {
        return Ok(Resolved::new(go, "project go.mod tool").args(&["tool", "gopls"]));
    }
    find("gopls")
        .map(|binary| Resolved::new(binary, "system"))
        .ok_or_else(|| Missing {
            install: "go install golang.org/x/tools/gopls@latest".into(),
        })
}

/// Whether `go.mod` has `tool <path>`, alone or in a `tool ( … )` block.
fn declares_go_tool(go_mod: &str, path: &str) -> bool {
    let mut in_block = false;
    for line in go_mod.lines().map(str::trim) {
        if in_block {
            if line == ")" {
                in_block = false;
            } else if line == path {
                return true;
            }
        } else if line == "tool (" {
            in_block = true;
        } else if line.strip_prefix("tool ").map(str::trim) == Some(path) {
            return true;
        }
    }
    false
}

/// A Python server from the project's virtualenv, or else the system, told
/// to resolve imports with the virtualenv's interpreter. The worktree has
/// no virtualenv of its own.
fn python(checkout: &Path) -> Result<Resolved, Missing> {
    const COMMANDS: &[(&str, &[&str])] = &[
        ("basedpyright-langserver", &["--stdio"]),
        ("pyright-langserver", &["--stdio"]),
        ("pylsp", &[]),
    ];
    let venv = [".venv", "venv"]
        .map(|dir| checkout.join(dir))
        .into_iter()
        .find(|dir| dir.join("bin/python").exists());
    let from_venv = venv.as_ref().and_then(|venv| {
        COMMANDS.iter().find_map(|(binary, args)| {
            let path = venv.join("bin").join(binary);
            path.is_file()
                .then(|| Resolved::new(path, "project .venv").args(args))
        })
    });
    let mut resolved = from_venv
        .or_else(|| {
            COMMANDS.iter().find_map(|(binary, args)| {
                find(binary).map(|path| Resolved::new(path, "system").args(args))
            })
        })
        .ok_or_else(|| Missing {
            install: if MACOS {
                "brew install basedpyright".into()
            } else {
                "pip install basedpyright".into()
            },
        })?;
    if let Some(venv) = venv {
        resolved.configuration = json!({
            "python": {"pythonPath": venv.join("bin/python")},
            "pylsp": {"plugins": {"jedi": {"environment": venv}}},
        });
        if resolved.source == "system" {
            resolved.source = "system, with project .venv".into();
        }
    }
    Ok(resolved)
}

/// A binary in the search directories.
fn find(binary: &str) -> Option<PathBuf> {
    search_dirs()
        .into_iter()
        .map(|dir| dir.join(binary))
        .find(|path| path.is_file())
}

/// Where servers and their runtimes usually live. Apps launched from
/// Finder get a minimal `PATH` that excludes Homebrew, language toolchains,
/// and Node version managers, so `PATH` alone isn't enough.
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    dirs.extend(
        [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/usr/bin",
            "/nix/var/nix/profiles/default/bin",
            "/run/current-system/sw/bin",
        ]
        .map(PathBuf::from),
    );
    if let Some(home) = dirs::home_dir() {
        dirs.extend(
            [
                ".cargo/bin",
                "go/bin",
                ".local/bin",
                ".nix-profile/bin",
                ".local/share/mise/shims",
                ".asdf/shims",
                ".volta/bin",
                ".bun/bin",
            ]
            .map(|dir| home.join(dir)),
        );
    }
    dirs
}

/// `PATH` for server processes: the search directories, so a server that's
/// a script (`#!/usr/bin/env node`) finds its interpreter.
pub(super) fn search_path() -> OsString {
    std::env::join_paths(search_dirs()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_go_tool_directives() {
        let gopls = "golang.org/x/tools/gopls";
        assert!(declares_go_tool(
            "module a\n\ntool golang.org/x/tools/gopls\n",
            gopls
        ));
        assert!(declares_go_tool(
            "tool (\n\texample.com/x\n\tgolang.org/x/tools/gopls\n)\n",
            gopls
        ));
        assert!(!declares_go_tool(
            "require golang.org/x/tools/gopls v0.1.0\n",
            gopls
        ));
    }

    #[test]
    fn reads_vscode_settings_with_comments() {
        let settings = r#"{
          // Editor tooling.
          "typescript.tsserver.maxTsServerMemory": 8192, /* MB */
          "url": "https://example.com/a,", // not a comment inside strings
          "quote": "say \"hi\"",
          "list": [1, 2,],
        }"#;
        let value: Value = serde_json::from_str(&strip_jsonc(settings)).unwrap();
        assert_eq!(value["typescript.tsserver.maxTsServerMemory"], 8192);
        assert_eq!(value["url"], "https://example.com/a,");
        assert_eq!(value["quote"], "say \"hi\"");
        assert_eq!(value["list"], json!([1, 2]));
    }

    #[test]
    fn finds_pnpm_aliased_typescript() {
        let root = std::env::temp_dir().join(format!("docket-tsserver-{}", std::process::id()));
        let alias = root.join("node_modules/typescript");
        let store = root.join("node_modules/.pnpm/typescript@6.0.2/node_modules/typescript/lib");
        fs::create_dir_all(alias.join("lib")).unwrap();
        fs::create_dir_all(&store).unwrap();
        fs::write(
            alias.join("package.json"),
            r#"{"name": "@typescript/typescript6", "version": "6.0.2"}"#,
        )
        .unwrap();
        fs::write(store.join("tsserver.js"), "").unwrap();

        let (path, version) = project_tsserver(&root).unwrap();
        assert!(path.ends_with("typescript@6.0.2/node_modules/typescript/lib/tsserver.js"));
        assert_eq!(version, "6.0.2");
        fs::remove_dir_all(&root).unwrap();
    }
}
