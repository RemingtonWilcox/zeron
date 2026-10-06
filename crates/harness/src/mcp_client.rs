//! A client for one stdio MCP server this process starts: the handshake,
//! `tools/list` and `tools/call` over the shared JSON-RPC client. The server
//! runs until its [`McpClient`] is dropped.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

use crate::HarnessError;
use crate::jsonrpc::{Incoming, RpcClient};
use crate::process::{Child, Command, Stdio};

/// How long a server may take to start and list its tools (uv may install
/// the server's dependencies first).
const START_TIMEOUT: Duration = Duration::from_secs(60);

pub struct McpClient {
    rpc: RpcClient,
    _child: Child,
}

impl McpClient {
    /// Starts `command` and completes the MCP handshake. Returns the client
    /// and the server's tools as its `tools/list` spelled them.
    pub async fn start(
        command: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
    ) -> Result<(Self, Vec<Value>), String> {
        let exe =
            crate::executable::find_on_paths(command, Vec::new()).unwrap_or_else(|| command.into());
        let mut cmd = Command::new(&exe);
        cmd.args(args);
        crate::compose_child_path(&mut cmd, &exe);
        for (key, value) in env {
            cmd.env(key, value);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|error| format!("{command}: {error}"))?;
        let stderr = crate::StderrTail::default();
        if let Some(pipe) = child.stderr.take() {
            let tail = stderr.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(pipe).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tail.push(&line);
                }
                tail.close();
            });
        }
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(format!("{command} has no stdio"));
        };
        let (rpc, incoming) = RpcClient::new(stdin, stdout);
        tokio::spawn(decline(rpc.clone(), incoming));
        let handshake = async {
            rpc.request(
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": { "name": "zeron", "version": env!("CARGO_PKG_VERSION") },
                }),
            )
            .await?;
            rpc.notify("notifications/initialized", None);
            let mut tools = Vec::new();
            let mut params = json!({});
            loop {
                let page = rpc.request("tools/list", params).await?;
                tools.extend(page["tools"].as_array().into_iter().flatten().cloned());
                match page["nextCursor"].as_str() {
                    Some(cursor) => params = json!({ "cursor": cursor }),
                    None => return Ok::<_, HarnessError>(tools),
                }
            }
        };
        match tokio::time::timeout(START_TIMEOUT, handshake).await {
            Ok(Ok(tools)) => Ok((Self { rpc, _child: child }, tools)),
            Ok(Err(_)) if rpc.is_closed() => {
                stderr.wait_closed().await;
                let status = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
                let status = status.ok().and_then(Result::ok);
                Err(crate::crash_message(command, status, &stderr))
            }
            Ok(Err(error)) => Err(error.to_string()),
            Err(_) => Err(format!(
                "{command} did not answer within {}s",
                START_TIMEOUT.as_secs()
            )),
        }
    }

    /// False once the server has exited.
    pub fn is_running(&self) -> bool {
        !self.rpc.is_closed()
    }

    /// The server's `tools/call` result, as sent.
    pub async fn call(&self, tool: &str, arguments: Value) -> Result<Value, String> {
        self.rpc
            .request(
                "tools/call",
                json!({ "name": tool, "arguments": arguments }),
            )
            .await
            .map_err(|error| error.to_string())
    }
}

/// Zeron offers a server no client capabilities: its requests (roots,
/// sampling) are declined and its notifications dropped.
async fn decline(rpc: RpcClient, mut incoming: mpsc::Receiver<Incoming>) {
    while let Some(message) = incoming.recv().await {
        if let Incoming::Request { id, method, .. } = message {
            rpc.respond_error(&id, -32601, &format!("unsupported: {method}"));
        }
    }
}
