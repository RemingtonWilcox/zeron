//! Claude Desktop extensions (MCPB bundles) as MCP server launches. Claude
//! Desktop unpacks each bundle into `Claude Extensions/<id>` and keeps its
//! enable switch and user config in `Claude Extensions Settings/<id>.json`.
//! Each manifest's `mcp_config` is resolved the way the MCPB reference
//! implementation does (platform overrides, `${…}` variables, array
//! expansion).

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::Value;

/// Claude Desktop's data folder on this device. There is no official Claude
/// Desktop on Linux.
pub fn desktop_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("APPDATA").map(|dir| PathBuf::from(dir).join("Claude"))
    } else if cfg!(target_os = "macos") {
        crate::executable::home_dir().map(|home| home.join("Library/Application Support/Claude"))
    } else {
        None
    }
}

/// Claude Code's user config file (`$CLAUDE_CONFIG_DIR/.claude.json` or
/// `~/.claude.json`), where user and local scope MCP servers live.
pub fn claude_json() -> PathBuf {
    match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir).join(".claude.json"),
        None => crate::executable::home_or_current_dir().join(".claude.json"),
    }
}

/// An enabled extension and the stdio MCP server it runs.
#[derive(Debug, Clone, PartialEq)]
pub struct Extension {
    /// The manifest name, kept to the characters tool names accept.
    pub name: String,
    pub description: String,
    /// Tool names the manifest declares; often empty.
    pub tools: Vec<String>,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

/// The enabled extensions in `desktop`, by folder name. A name the user's
/// Claude Code config for `cwd` already has (compared case-insensitively)
/// skips the extension, so a server added by hand never runs twice.
pub fn extensions(desktop: &Path, claude_json: &Path, cwd: &Path) -> Vec<Extension> {
    let mut extensions = Vec::new();
    let Ok(entries) = std::fs::read_dir(desktop.join("Claude Extensions")) else {
        return extensions;
    };
    let mut taken = configured_names(claude_json, cwd);
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    dirs.sort();
    for dir in dirs {
        let Some(id) = dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let settings = read_json(
            &desktop
                .join("Claude Extensions Settings")
                .join(format!("{id}.json")),
        );
        if settings["isEnabled"] != true || !settings["orgBlockedReason"].is_null() {
            continue;
        }
        match extension(&dir, &read_json(&dir.join("manifest.json")), &settings) {
            Ok(extension) if !taken.insert(extension.name.to_lowercase()) => {
                skip(id, "a server with this name is already configured")
            }
            Ok(extension) => extensions.push(extension),
            Err(reason) => skip(id, reason),
        }
    }
    extensions
}

fn read_json(path: &Path) -> Value {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Lowercased names of the MCP servers the user configured for Claude Code in
/// `cwd`: user and local scope in `.claude.json`, project scope in `.mcp.json`.
fn configured_names(claude_json: &Path, cwd: &Path) -> HashSet<String> {
    let config = read_json(claude_json);
    let project = read_json(&cwd.join(".mcp.json"));
    let key = cwd.to_string_lossy().replace('\\', "/");
    [
        &config["mcpServers"],
        &config["projects"][key.as_str()]["mcpServers"],
        &project["mcpServers"],
    ]
    .into_iter()
    .filter_map(Value::as_object)
    .flat_map(|servers| servers.keys())
    .map(|name| name.to_lowercase())
    .collect()
}

/// Logs each skipped extension once per process.
fn skip(id: &str, reason: &str) {
    static LOGGED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
    if LOGGED
        .lock()
        .is_ok_and(|mut logged| logged.insert(id.to_owned()))
    {
        tracing::info!(target: "zeron_harness::claude", extension = id, "Claude Desktop extension skipped: {reason}");
    }
}

/// One extension in `dir`. `settings` is its Claude Desktop settings file;
/// `userConfig` there overrides the manifest's defaults.
fn extension(dir: &Path, manifest: &Value, settings: &Value) -> Result<Extension, &'static str> {
    let name: String = manifest["name"]
        .as_str()
        .unwrap_or_default()
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => c,
            _ => '-',
        })
        .collect();
    if name.is_empty() {
        return Err("the manifest has no name");
    }
    let kind = manifest["server"]["type"].as_str().unwrap_or_default();
    if !matches!(kind, "node" | "python" | "uv" | "binary") {
        return Err("the server type is not one Zeron can run");
    }
    let mut config = manifest["server"]["mcp_config"].clone();
    let platform = if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
    let overrides = config["platform_overrides"][platform].clone();
    for key in ["command", "args"] {
        if !overrides[key].is_null() {
            config[key] = overrides[key].clone();
        }
    }
    if let Some(env) = overrides["env"].as_object() {
        for (key, value) in env {
            config["env"][key] = value.clone();
        }
    }
    let Some(command) = config["command"].as_str() else {
        return Err("the manifest has no mcp_config command");
    };

    let home = crate::executable::home_or_current_dir();
    let separator = std::path::MAIN_SEPARATOR_STR;
    let mut vars: Vec<(String, Vec<String>)> = [
        ("__dirname", dir.to_path_buf()),
        ("HOME", home.clone()),
        ("DESKTOP", home.join("Desktop")),
        ("DOCUMENTS", home.join("Documents")),
        ("DOWNLOADS", home.join("Downloads")),
        ("pathSeparator", separator.into()),
        ("/", separator.into()),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), vec![value.to_string_lossy().into_owned()]))
    .collect();
    let mut user_config = Vec::new();
    for (key, option) in manifest["user_config"].as_object().into_iter().flatten() {
        let value = settings["userConfig"]
            .get(key)
            .filter(|value| !value.is_null())
            .unwrap_or(&option["default"]);
        let values = strings(value);
        if option["required"] == true && (values.is_empty() || values.iter().any(String::is_empty))
        {
            return Err("a required setting has no value");
        }
        if values.iter().any(|v| v.starts_with("__encrypted__:")) {
            return Err("a sensitive setting is encrypted by Claude Desktop");
        }
        let values = values.iter().map(|v| replace(v, &vars)).collect();
        user_config.push((format!("user_config.{key}"), values));
    }
    vars.extend(user_config);

    // An argument that is exactly one variable expands to all of its values.
    let mut args: Vec<String> = config["args"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .flat_map(
            |arg| match vars.iter().find(|(key, _)| arg == format!("${{{key}}}")) {
                Some((_, values)) => values.clone(),
                None => vec![replace(arg, &vars)],
            },
        )
        .collect();
    let mut command = replace(command, &vars);
    match kind {
        // Claude Desktop runs uv servers in the extension folder; uv is told
        // instead, so the server's working directory does not matter.
        "uv" => {
            command = "uv".into();
            args.splice(
                0..0,
                ["--directory".into(), dir.to_string_lossy().into_owned()],
            );
        }
        "binary" if Path::new(&command).is_relative() => {
            command = dir.join(command).to_string_lossy().into_owned();
        }
        _ => {}
    }
    let env = config["env"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(key, value)| Some((key.clone(), replace(value.as_str()?, &vars))))
        .collect();
    Ok(Extension {
        name,
        description: manifest["description"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        tools: manifest["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|tool| Some(tool["name"].as_str()?.to_owned()))
            .collect(),
        command,
        args,
        env,
    })
}

/// A user config value as strings: arrays (multiple selections) item by item,
/// booleans and numbers spelled out.
fn strings(value: &Value) -> Vec<String> {
    match value {
        Value::Null => Vec::new(),
        Value::String(text) => vec![text.clone()],
        Value::Array(items) => items.iter().flat_map(strings).collect(),
        other => vec![other.to_string()],
    }
}

/// Substitutes every single-valued `${key}` in `text`.
fn replace(text: &str, vars: &[(String, Vec<String>)]) -> String {
    vars.iter().fold(text.to_owned(), |text, (key, values)| {
        match values.as_slice() {
            [value] => text.replace(&format!("${{{key}}}"), value),
            _ => text,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A Claude Desktop folder holding one extension `id`.
    fn install(desktop: &Path, id: &str, manifest: Value, settings: Value) -> PathBuf {
        let dir = desktop.join("Claude Extensions").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
        let settings_dir = desktop.join("Claude Extensions Settings");
        std::fs::create_dir_all(&settings_dir).unwrap();
        std::fs::write(
            settings_dir.join(format!("{id}.json")),
            settings.to_string(),
        )
        .unwrap();
        dir
    }

    fn enabled() -> Value {
        json!({ "isEnabled": true })
    }

    fn read(desktop: &Path) -> BTreeMap<String, Extension> {
        extensions(desktop, &desktop.join("missing.json"), desktop)
            .into_iter()
            .map(|extension| (extension.name.clone(), extension))
            .collect()
    }

    #[test]
    fn manifests_list_their_description_and_tools() {
        let desktop = tempfile::tempdir().unwrap();
        let manifest = json!({
            "name": "Blender",
            "description": "Drive Blender",
            "tools": [{ "name": "get_scene_info" }, { "name": "run_python" }],
            "server": {
                "type": "uv",
                "entry_point": "blmcp/__init__.py",
                "mcp_config": { "command": "uv", "args": ["run", "blender-mcp"] }
            }
        });
        let dir = install(desktop.path(), "ant.blender", manifest, enabled());
        let extensions = read(desktop.path());
        let blender = &extensions["Blender"];
        assert_eq!(blender.description, "Drive Blender");
        assert_eq!(blender.tools, ["get_scene_info", "run_python"]);
        // Claude Desktop runs uv servers in their folder.
        assert_eq!(blender.command, "uv");
        assert_eq!(
            blender.args,
            [
                "--directory".to_owned(),
                dir.to_string_lossy().into_owned(),
                "run".into(),
                "blender-mcp".into()
            ]
        );
    }

    #[test]
    fn node_python_and_binary_resolve_placeholders() {
        let desktop = tempfile::tempdir().unwrap();
        let node = install(
            desktop.path(),
            "node",
            json!({
                "name": "Files",
                "user_config": {
                    "dirs": { "type": "directory", "multiple": true, "default": ["${HOME}${/}Desktop"] },
                    "key": { "type": "string", "required": true },
                    "verbose": { "type": "boolean", "default": false }
                },
                "server": { "type": "node", "mcp_config": {
                    "command": "node",
                    "args": ["${__dirname}/server/index.js", "${user_config.dirs}"],
                    "env": { "API_KEY": "${user_config.key}", "VERBOSE": "${user_config.verbose}" }
                }}
            }),
            json!({ "isEnabled": true, "userConfig": { "key": "k-1", "dirs": ["/a", "/b"] } }),
        );
        let python = install(
            desktop.path(),
            "python",
            json!({ "name": "Py Tools", "server": { "type": "python", "mcp_config": {
                "command": "python",
                "args": ["${__dirname}/server/main.py", "${user_config.unset}"],
                "env": { "PYTHONPATH": "${__dirname}/server/lib" }
            }}}),
            enabled(),
        );
        let binary = install(
            desktop.path(),
            "binary",
            json!({ "name": "bin", "server": { "type": "binary", "mcp_config": {
                "command": "server/tool",
                "platform_overrides": {
                    "win32": { "command": "server/tool.exe" },
                    "darwin": { "command": "server/tool" },
                    "linux": { "command": "server/tool" }
                }
            }}}),
            enabled(),
        );
        let extensions = read(desktop.path());

        let files = &extensions["Files"];
        assert_eq!(files.command, "node");
        assert_eq!(
            files.args,
            [
                format!("{}/server/index.js", node.to_string_lossy()),
                "/a".into(),
                "/b".into()
            ]
        );
        assert_eq!(
            files.env,
            BTreeMap::from([
                ("API_KEY".to_owned(), "k-1".to_owned()),
                ("VERBOSE".to_owned(), "false".to_owned())
            ])
        );

        // Names are kept to the characters tool names accept; a variable
        // with no value stays as written, as in Claude Desktop.
        let py = &extensions["Py-Tools"];
        assert_eq!(
            py.args,
            [
                format!("{}/server/main.py", python.to_string_lossy()),
                "${user_config.unset}".into()
            ]
        );
        assert_eq!(
            py.env["PYTHONPATH"],
            format!("{}/server/lib", python.to_string_lossy())
        );

        let tool = if cfg!(windows) {
            "server/tool.exe"
        } else {
            "server/tool"
        };
        assert_eq!(
            extensions["bin"].command,
            binary.join(tool).to_string_lossy()
        );
    }

    #[test]
    fn defaults_fill_unset_user_config() {
        let desktop = tempfile::tempdir().unwrap();
        install(
            desktop.path(),
            "files",
            json!({
                "name": "files",
                "user_config": { "dirs": { "type": "directory", "multiple": true, "required": true, "default": ["${HOME}${/}Desktop"] } },
                "server": { "type": "node", "mcp_config": { "command": "node", "args": ["${user_config.dirs}"] } }
            }),
            enabled(),
        );
        let home = crate::executable::home_or_current_dir();
        assert_eq!(
            read(desktop.path())["files"].args,
            [home.join("Desktop").to_string_lossy().into_owned()]
        );
    }

    #[test]
    fn disabled_unconfigured_encrypted_and_unrunnable_extensions_are_skipped() {
        let desktop = tempfile::tempdir().unwrap();
        let runnable = |name: &str| json!({ "name": name, "server": { "type": "node", "mcp_config": { "command": "node" } } });
        install(
            desktop.path(),
            "off",
            runnable("off"),
            json!({ "isEnabled": false }),
        );
        install(
            desktop.path(),
            "blocked",
            runnable("blocked"),
            json!({ "isEnabled": true, "orgBlockedReason": "policy" }),
        );
        let needs_key = json!({
            "name": "needs-key",
            "user_config": { "key": { "type": "string", "required": true } },
            "server": { "type": "node", "mcp_config": { "command": "node" } }
        });
        install(desktop.path(), "needs-key", needs_key.clone(), enabled());
        let mut secret = needs_key;
        secret["name"] = "secret".into();
        install(
            desktop.path(),
            "secret",
            secret,
            json!({ "isEnabled": true, "userConfig": { "key": "__encrypted__:AAAA" } }),
        );
        install(
            desktop.path(),
            "no-command",
            json!({ "name": "no-command", "server": { "type": "node" } }),
            enabled(),
        );
        install(
            desktop.path(),
            "odd",
            json!({ "name": "odd", "server": { "type": "wasm", "mcp_config": { "command": "x" } } }),
            enabled(),
        );
        assert!(read(desktop.path()).is_empty());
    }

    #[test]
    fn names_the_user_already_configured_are_skipped() {
        let desktop = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        for name in ["Blender", "Local", "Project", "kept"] {
            install(
                desktop.path(),
                name,
                json!({ "name": name, "server": { "type": "node", "mcp_config": { "command": "node" } } }),
                enabled(),
            );
        }
        let key = cwd.path().to_string_lossy().replace('\\', "/");
        let claude_json = desktop.path().join(".claude.json");
        std::fs::write(
            &claude_json,
            json!({
                "mcpServers": { "blender": {} },
                "projects": { (key): { "mcpServers": { "local": {} } } }
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            cwd.path().join(".mcp.json"),
            json!({ "mcpServers": { "PROJECT": {} } }).to_string(),
        )
        .unwrap();
        let names: Vec<String> = extensions(desktop.path(), &claude_json, cwd.path())
            .into_iter()
            .map(|extension| extension.name)
            .collect();
        assert_eq!(names, ["kept"]);
    }
}
