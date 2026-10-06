//! Connectors: the user's enabled Claude Desktop extensions, offered to the
//! agent but started only when it enables one. A started connector runs as a
//! child of this server until it exits; its tools join this server's list
//! as `<connector>__<tool>` and calls to them are forwarded.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use zeron_harness::claude::desktop_extensions::{self, Extension};
use zeron_harness::mcp_client::McpClient;

pub(crate) struct Connectors {
    desktop: Option<PathBuf>,
    claude_json: PathBuf,
    /// The agent's working directory, which picks its Claude Code config.
    cwd: PathBuf,
    /// Started connectors by lowercased name.
    running: Mutex<BTreeMap<String, Arc<Running>>>,
    /// Serializes starts, so a connector enabled twice at once starts once.
    starting: tokio::sync::Mutex<()>,
    /// Bumped whenever the tool list grows.
    revision: AtomicU64,
}

struct Running {
    name: String,
    client: McpClient,
    /// The connector's tools as it listed them.
    tools: Vec<Value>,
}

impl Running {
    /// The tools under this server's names, `<connector>__<tool>` in lower case.
    fn tools(&self) -> impl Iterator<Item = Value> + '_ {
        let prefix = self.name.to_lowercase();
        self.tools.iter().map(move |tool| {
            let mut tool = tool.clone();
            tool["name"] = format!("{prefix}__{}", tool["name"].as_str().unwrap_or("")).into();
            tool
        })
    }
}

impl Connectors {
    pub(crate) fn new(desktop: Option<PathBuf>, claude_json: PathBuf, cwd: PathBuf) -> Self {
        Self {
            desktop,
            claude_json,
            cwd,
            running: Mutex::default(),
            starting: tokio::sync::Mutex::default(),
            revision: AtomicU64::default(),
        }
    }

    pub(crate) fn from_env() -> Self {
        Self::new(
            desktop_extensions::desktop_dir(),
            desktop_extensions::claude_json(),
            std::env::current_dir().unwrap_or_default(),
        )
    }

    fn available(&self) -> Vec<Extension> {
        self.desktop.as_deref().map_or_else(Vec::new, |desktop| {
            desktop_extensions::extensions(desktop, &self.claude_json, &self.cwd)
        })
    }

    fn running(&self, name: &str) -> Option<Arc<Running>> {
        let running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        running.get(&name.to_lowercase()).cloned()
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    /// One server-instructions line naming the connectors, if there are any.
    pub(crate) fn instructions(&self) -> Option<String> {
        let names: Vec<String> = self.available().into_iter().map(|c| c.name).collect();
        (!names.is_empty()).then(|| {
            format!(
                "Tool connectors the user installed, off until enabled: {}. When a task \
                 needs one, call `connector_enable`; `connectors` lists what each offers.",
                names.join(", ")
            )
        })
    }

    pub(crate) fn list(&self) -> Value {
        let connectors: Vec<Value> = self
            .available()
            .into_iter()
            .map(|connector| {
                let running = self
                    .running(&connector.name)
                    .filter(|r| r.client.is_running());
                let tools: Vec<Value> = match &running {
                    Some(running) => running
                        .tools
                        .iter()
                        .map(|tool| tool["name"].clone())
                        .collect(),
                    None => connector.tools.into_iter().map(Value::from).collect(),
                };
                json!({
                    "name": connector.name,
                    "description": connector.description,
                    "tools": tools,
                    "enabled": running.is_some(),
                })
            })
            .collect();
        json!({ "connectors": connectors })
    }

    /// Starts `name` unless it is running, and returns its tools.
    pub(crate) async fn enable(&self, name: &str) -> anyhow::Result<Value> {
        let _starting = self.starting.lock().await;
        let running = match self.running(name).filter(|r| r.client.is_running()) {
            Some(running) => running,
            None => {
                let available = self.available();
                let Some(connector) = available.iter().find(|c| c.name.eq_ignore_ascii_case(name))
                else {
                    let mut names: Vec<&str> = available.iter().map(|c| c.name.as_str()).collect();
                    if names.is_empty() {
                        names.push("none");
                    }
                    anyhow::bail!(
                        "unknown connector: {name} (available: {})",
                        names.join(", ")
                    );
                };
                let (client, tools) =
                    McpClient::start(&connector.command, &connector.args, &connector.env)
                        .await
                        .map_err(|error| {
                            anyhow::anyhow!("{} did not start: {error}", connector.name)
                        })?;
                let running = Arc::new(Running {
                    name: connector.name.clone(),
                    client,
                    tools,
                });
                self.running
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(connector.name.to_lowercase(), running.clone());
                self.revision.fetch_add(1, Ordering::AcqRel);
                running
            }
        };
        Ok(json!({
            "connector": running.name,
            "tools": running.tools().collect::<Vec<_>>(),
            "note": "These tools are now on this server's tool list, under its prefix like the other zeron tools (mcp__zeron__… in Claude Code). If they do not appear, call them through `connector_call`.",
        }))
    }

    /// Every running connector's tools.
    pub(crate) fn tools(&self) -> Vec<Value> {
        let running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        running.values().flat_map(|r| r.tools()).collect()
    }

    /// `(connector, tool)` for one of [`Self::tools`].
    pub(crate) fn owner(&self, name: &str) -> Option<(String, String)> {
        let running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        running.iter().find_map(|(prefix, r)| {
            let tool = name.strip_prefix(prefix.as_str())?.strip_prefix("__")?;
            Some((r.name.clone(), tool.to_owned()))
        })
    }

    /// Forwards a call to a running connector; `Ok` is its `tools/call` result.
    pub(crate) async fn call(
        &self,
        connector: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<Value, String> {
        let running = self.running(connector).ok_or_else(|| {
            format!("connector {connector} is not enabled; call connector_enable first")
        })?;
        running.client.call(tool, arguments).await
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    use super::*;
    use crate::{Origin, Tools, Zeron};

    /// This test binary, run as a connector: a stdio MCP server with one
    /// `echo` tool that logs each start to `$ZERON_FAKE_CONNECTOR`.
    #[test]
    fn fake_connector() {
        let Ok(log) = std::env::var("ZERON_FAKE_CONNECTOR") else {
            return;
        };
        let mut starts = std::fs::read_to_string(&log).unwrap_or_default();
        starts.push_str("start\n");
        std::fs::write(&log, starts).unwrap();
        // The test harness may have left a partial line on stdout.
        println!();
        for line in std::io::stdin().lines() {
            let request: Value = serde_json::from_str(&line.unwrap()).unwrap();
            let result = match request["method"].as_str() {
                Some("initialize") => {
                    json!({ "protocolVersion": "2025-06-18", "capabilities": {} })
                }
                Some("tools/list") => json!({ "tools": [{
                    "name": "echo", "description": "Echoes its arguments.",
                    "inputSchema": { "type": "object" }
                }] }),
                Some("tools/call") => json!({
                    "content": [{ "type": "text", "text": request["params"]["arguments"].to_string() }]
                }),
                _ => continue,
            };
            println!(
                "{}",
                json!({ "jsonrpc": "2.0", "id": request["id"], "result": result })
            );
        }
    }

    /// A Claude Desktop folder with the fake connector (`Fake`) and a
    /// disabled extension; returns it and the connector's start log.
    fn desktop() -> (tempfile::TempDir, PathBuf) {
        let desktop = tempfile::tempdir().unwrap();
        let log = desktop.path().join("starts.log");
        let install = |id: &str, manifest: Value, enabled: bool| {
            let dir = desktop.path().join("Claude Extensions").join(id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
            let settings = desktop.path().join("Claude Extensions Settings");
            std::fs::create_dir_all(&settings).unwrap();
            let settings = settings.join(format!("{id}.json"));
            std::fs::write(settings, json!({ "isEnabled": enabled }).to_string()).unwrap();
        };
        let exe = std::env::current_exe().unwrap();
        install(
            "fake",
            json!({
                "name": "Fake",
                "description": "A fake connector.",
                "tools": [{ "name": "echo" }],
                "server": { "type": "binary", "mcp_config": {
                    "command": exe.to_string_lossy(),
                    "args": ["--exact", "connectors::tests::fake_connector", "--nocapture", "-q"],
                    "env": { "ZERON_FAKE_CONNECTOR": log.to_string_lossy() }
                }}
            }),
            true,
        );
        install(
            "off",
            json!({ "name": "Off", "server": { "type": "node", "mcp_config": { "command": "node" } } }),
            false,
        );
        (desktop, log)
    }

    fn tools(desktop: &Path) -> Arc<Tools> {
        let zeron = Zeron::new("ws://127.0.0.1:9".into(), Origin::default());
        let connectors = Connectors::new(
            Some(desktop.to_path_buf()),
            desktop.join(".claude.json"),
            desktop.to_path_buf(),
        );
        Arc::new(Tools::new(Arc::new(zeron)).with_connectors(connectors))
    }

    /// An MCP client session with [`crate::jsonrpc::serve`].
    struct Session {
        write: tokio::io::WriteHalf<tokio::io::DuplexStream>,
        lines: tokio::io::Lines<BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>,
        next_id: i64,
    }

    impl Session {
        fn new(tools: Arc<Tools>) -> Self {
            let (client, server) = tokio::io::duplex(1 << 16);
            let (input, output) = tokio::io::split(server);
            tokio::spawn(crate::jsonrpc::serve(tools, input, output));
            let (read, write) = tokio::io::split(client);
            let lines = BufReader::new(read).lines();
            Self {
                write,
                lines,
                next_id: 0,
            }
        }

        /// Sends a request; returns its result and the lines that follow it
        /// within a moment.
        async fn request(&mut self, method: &str, params: Value) -> (Value, Vec<Value>) {
            self.next_id += 1;
            let id = self.next_id;
            let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
            self.write
                .write_all(format!("{request}\n").as_bytes())
                .await
                .unwrap();
            let read = async {
                loop {
                    let line = self.lines.next_line().await.unwrap().unwrap();
                    let line: Value = serde_json::from_str(&line).unwrap();
                    if line["id"] == id {
                        return line["result"].clone();
                    }
                }
            };
            let result = tokio::time::timeout(std::time::Duration::from_secs(30), read)
                .await
                .expect("a response");
            let mut after = Vec::new();
            let moment = std::time::Duration::from_millis(200);
            while let Ok(line) = tokio::time::timeout(moment, self.lines.next_line()).await {
                after.push(serde_json::from_str(&line.unwrap().unwrap()).unwrap());
            }
            (result, after)
        }

        async fn call(&mut self, name: &str, arguments: Value) -> (Value, Vec<Value>) {
            let params = json!({ "name": name, "arguments": arguments });
            self.request("tools/call", params).await
        }
    }

    fn text(result: &Value) -> &str {
        result["content"][0]["text"].as_str().unwrap()
    }

    fn names(listed: &Value) -> Vec<&str> {
        let tools = listed["tools"].as_array().unwrap();
        tools.iter().map(|t| t["name"].as_str().unwrap()).collect()
    }

    #[tokio::test]
    async fn enabled_extensions_are_listed_without_starting() {
        let (desktop, log) = desktop();
        let mut session = Session::new(tools(desktop.path()));
        let (init, _) = session.request("initialize", json!({})).await;
        assert_eq!(init["capabilities"]["tools"]["listChanged"], true);
        let instructions = init["instructions"].as_str().unwrap();
        assert!(instructions.contains("off until enabled: Fake."));
        let (listed, _) = session.call("connectors", json!({})).await;
        assert_eq!(
            listed["structuredContent"],
            json!({ "connectors": [{
                "name": "Fake", "description": "A fake connector.",
                "tools": ["echo"], "enabled": false
            }] })
        );
        assert!(!log.exists());
    }

    #[tokio::test]
    async fn enabling_starts_a_connector_once_and_forwards_its_calls() {
        let (desktop, log) = desktop();
        let mut session = Session::new(tools(desktop.path()));
        let (listed, _) = session.request("tools/list", json!({})).await;
        assert!(!names(&listed).contains(&"fake__echo"));

        let (enabled, after) = session
            .call("connector_enable", json!({ "name": "fake" }))
            .await;
        assert_eq!(enabled["isError"], false, "{enabled}");
        let tools = &enabled["structuredContent"]["tools"];
        assert_eq!(tools[0]["name"], "fake__echo");
        assert_eq!(tools[0]["inputSchema"], json!({ "type": "object" }));
        assert!(text(&enabled).contains("fake__echo"));
        assert_eq!(
            after,
            [json!({ "jsonrpc": "2.0", "method": "notifications/tools/list_changed" })]
        );
        let (listed, _) = session.request("tools/list", json!({})).await;
        assert!(names(&listed).contains(&"fake__echo"));

        let (echoed, _) = session.call("fake__echo", json!({ "x": 1 })).await;
        assert_eq!(
            echoed,
            json!({ "content": [{ "type": "text", "text": "{\"x\":1}" }] })
        );
        let arguments = json!({ "connector": "fake", "tool": "echo", "arguments": { "y": 2 } });
        let (echoed, _) = session.call("connector_call", arguments).await;
        assert_eq!(text(&echoed), "{\"y\":2}");

        let (again, after) = session
            .call("connector_enable", json!({ "name": "Fake" }))
            .await;
        assert_eq!(again["structuredContent"]["tools"], *tools);
        assert!(after.is_empty(), "{after:?}");
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "start\n");
        let (listed, _) = session.call("connectors", json!({})).await;
        assert_eq!(
            listed["structuredContent"]["connectors"][0]["enabled"],
            true
        );
    }

    #[tokio::test]
    async fn unknown_and_disabled_connectors_are_clear_errors() {
        let (desktop, log) = desktop();
        let mut session = Session::new(tools(desktop.path()));
        for name in ["nope", "Off"] {
            let (result, after) = session
                .call("connector_enable", json!({ "name": name }))
                .await;
            assert_eq!(result["isError"], true);
            assert_eq!(
                text(&result),
                format!("unknown connector: {name} (available: Fake)")
            );
            assert!(after.is_empty());
        }
        let arguments = json!({ "connector": "Fake", "tool": "echo" });
        let (result, _) = session.call("connector_call", arguments).await;
        assert_eq!(
            text(&result),
            "connector Fake is not enabled; call connector_enable first"
        );
        assert!(!log.exists());
    }
}
