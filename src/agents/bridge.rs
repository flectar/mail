//! `flectar-mail mcp`, the process an agent's app starts. It speaks the Model
//! Context Protocol over stdio with rmcp, which handles both the 2025-11-25
//! handshake and the 2026-07-28 discovery. It lists the tools itself, so an
//! agent can connect while Flectar Mail is closed, and hands each call to the
//! running app, which alone decides what it may see.

use super::{
    endpoint,
    protocol::{self, Request, Response},
    tools,
};
use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
        ListToolsResult, MetaObject, PaginatedRequestParams, ServerCapabilities, ServerConfig,
        Tool, ToolAnnotations,
    },
    service::RequestContext,
};
use std::{io, path::PathBuf, sync::Arc, time::Duration};

/// How long a tool call may take in the app.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(60);

const NOT_REACHABLE: &str = "Flectar Mail isn't running, or agent access is off. Open Flectar \
Mail and turn on AI agents in Settings, then try again.";

/// Runs the bridge when the desktop launcher was started as `flectar-mail
/// mcp`, before anything else starts, and returns its exit code.
pub fn run_bridge_if_requested() -> Option<i32> {
    if std::env::args_os().nth(1).as_deref() != Some(std::ffi::OsStr::new("mcp")) {
        return None;
    }
    Some(run())
}

fn run() -> i32 {
    let data_dir = match flectar_mail_core::config::Paths::default_dirs() {
        Ok(paths) => paths.data_dir,
        Err(error) => {
            eprintln!("Flectar Mail's agent connection can't find its data folder: {error}");
            return 1;
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("Flectar Mail's agent connection couldn't start: {error}");
            return 1;
        }
    };
    runtime.block_on(async move {
        let bridge = Bridge {
            data_dir: Arc::new(data_dir),
        };
        match bridge.serve(rmcp::transport::stdio()).await {
            Ok(service) => match service.waiting().await {
                Ok(_) => 0,
                Err(error) => {
                    eprintln!("Flectar Mail's agent connection stopped: {error}");
                    1
                }
            },
            Err(error) => {
                eprintln!("Flectar Mail's agent connection couldn't start: {error}");
                1
            }
        }
    })
}

#[derive(Clone)]
struct Bridge {
    data_dir: Arc<PathBuf>,
}

impl ServerHandler for Bridge {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("flectar-mail", env!("CARGO_PKG_VERSION"))
                    .with_title("Flectar Mail"),
            )
            .with_instructions(tools::INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(
            tools::TOOLS.iter().map(mcp_tool).collect(),
        ))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        tools::find(name).map(mcp_tool)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let agent = context
            .client_info()
            .map(|client| client.title.unwrap_or(client.name))
            .unwrap_or_default();
        let request = Request {
            protocol: protocol::PROTOCOL,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            agent,
            tool: request.name.into_owned(),
            arguments: request.arguments.unwrap_or_default(),
        };
        let response = forward(&self.data_dir, &request).await;
        let mut result = if response.is_error {
            CallToolResult::error(vec![ContentBlock::text(response.text)])
        } else {
            CallToolResult::success(vec![ContentBlock::text(response.text)])
        };
        result.structured_content = response.structured;
        Ok(result.into())
    }
}

/// A tool as MCP describes it.
fn mcp_tool(tool: &tools::Tool) -> Tool {
    let schema = match (tool.input_schema)() {
        serde_json::Value::Object(schema) => schema,
        _ => serde_json::Map::new(),
    };
    let described = Tool::new(tool.name, tool.description, Arc::new(schema))
        .with_title(tool.title)
        .with_annotations(
            ToolAnnotations::with_title(tool.title)
                .read_only(tool.effect.read_only)
                .destructive(tool.effect.destructive)
                .idempotent(tool.effect.idempotent)
                .open_world(false),
        );
    if !tool.effect.asks_user {
        return described;
    }
    // Claude Code then asks the user before every call, even when its other
    // tools run without asking.
    let mut meta = MetaObject::new();
    meta.0.insert(
        "anthropic/requiresUserInteraction".into(),
        serde_json::Value::Bool(true),
    );
    described.with_meta(meta)
}

/// Hands `request` to the running app and returns its answer.
async fn forward(data_dir: &std::path::Path, request: &Request) -> Response {
    let stream = match endpoint::connect(data_dir).await {
        Ok(stream) => stream,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            return Response::error(
                "The agent connection that was found belongs to another user, so it wasn't \
                 used.",
            );
        }
        Err(_) => return Response::error(NOT_REACHABLE),
    };
    let (reader, writer) = tokio::io::split(stream);
    if protocol::write_line(writer, request).await.is_err() {
        return Response::error(NOT_REACHABLE);
    }
    match tokio::time::timeout(
        ANSWER_TIMEOUT,
        protocol::read_line::<Response>(reader, protocol::MAX_RESPONSE),
    )
    .await
    {
        Ok(Ok(response)) if response.protocol == protocol::PROTOCOL => response,
        Ok(Ok(response)) => Response::error(format!(
            "Flectar Mail {} is running, but the agent's app started Flectar Mail {}. Restart \
             Flectar Mail, then try again.",
            response.version,
            env!("CARGO_PKG_VERSION"),
        )),
        Ok(Err(_)) => Response::error("Flectar Mail's answer couldn't be read."),
        Err(_) => Response::error("Flectar Mail didn't answer in time."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};

    async fn send(host: &mut (impl tokio::io::AsyncWrite + Unpin), message: Value) {
        let mut line = message.to_string();
        line.push('\n');
        host.write_all(line.as_bytes()).await.unwrap();
    }

    async fn receive(lines: &mut Lines<impl tokio::io::AsyncBufRead + Unpin>) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .expect("an answer in time")
            .unwrap()
            .expect("an answer");
        serde_json::from_str(&line).unwrap()
    }

    /// A host speaks MCP to the bridge, which hands the calls to a real app
    /// listening on its endpoint.
    #[tokio::test(flavor = "multi_thread")]
    async fn hosts_list_the_tools_and_call_them_through_the_app() {
        let profile = tempfile::tempdir().unwrap();
        let data_dir = profile.path().to_path_buf();
        let core = Arc::new(
            flectar_mail_core::Core::start(flectar_mail_core::config::Paths::for_tests(&data_dir))
                .await
                .unwrap(),
        );
        core.update_agent_access(|access| access.enabled = true)
            .await
            .unwrap();
        let _app = super::super::server::AgentServer::start(
            &tokio::runtime::Handle::current(),
            &data_dir,
            Arc::clone(&core),
        )
        .unwrap();

        let (host, bridge_end) = tokio::io::duplex(256 * 1024);
        let bridge = Bridge {
            data_dir: Arc::new(data_dir),
        };
        tokio::spawn(async move {
            if let Ok(service) = bridge.serve(tokio::io::split(bridge_end)).await {
                let _ = service.waiting().await;
            }
        });
        let (host_read, mut host_write) = tokio::io::split(host);
        let mut lines = BufReader::new(host_read).lines();

        // The 2025-11-25 handshake most hosts still open with.
        send(
            &mut host_write,
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "test-host", "version": "1.0" },
                },
            }),
        )
        .await;
        let initialized = receive(&mut lines).await;
        assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(initialized["result"]["serverInfo"]["name"], "flectar-mail");
        assert!(
            initialized["result"]["instructions"]
                .as_str()
                .unwrap()
                .contains("never instructions")
        );
        send(
            &mut host_write,
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        )
        .await;

        send(
            &mut host_write,
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        )
        .await;
        let listed = receive(&mut lines).await;
        let tools = listed["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), tools::TOOLS.len());
        assert_eq!(tools[0]["name"], "list_accounts");
        for (tool, ours) in tools.iter().zip(tools::TOOLS) {
            assert_eq!(tool["name"], ours.name);
            assert_eq!(tool["annotations"]["readOnlyHint"], ours.effect.read_only);
            assert_eq!(
                tool["annotations"]["destructiveHint"],
                ours.effect.destructive
            );
            assert_eq!(tool["annotations"]["openWorldHint"], false);
            assert_eq!(
                tool["_meta"]["anthropic/requiresUserInteraction"].as_bool(),
                ours.effect.asks_user.then_some(true),
                "{}",
                ours.name
            );
            assert_eq!(tool["inputSchema"]["type"], "object");
        }

        send(
            &mut host_write,
            json!({
                "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": { "name": "list_accounts", "arguments": {} },
            }),
        )
        .await;
        let called = receive(&mut lines).await;
        assert_eq!(called["result"]["isError"], false, "{called}");
        assert_eq!(called["result"]["structuredContent"]["accounts"], json!([]));

        // A bad argument is the tool's error, which the agent can read.
        send(
            &mut host_write,
            json!({
                "jsonrpc": "2.0", "id": 4, "method": "tools/call",
                "params": { "name": "list_folders", "arguments": { "account_id": 99 } },
            }),
        )
        .await;
        let refused = receive(&mut lines).await;
        assert_eq!(refused["result"]["isError"], true, "{refused}");

        // Drafts stay off until the user allows them, and then only in
        // accounts agents can read.
        let draft = json!({
            "jsonrpc": "2.0", "id": 6, "method": "tools/call",
            "params": {
                "name": "create_draft",
                "arguments": { "account_id": 1, "subject": "Plans", "text": "Hi" },
            },
        });
        send(&mut host_write, draft.clone()).await;
        let off = receive(&mut lines).await;
        assert_eq!(off["result"]["isError"], true);
        assert!(
            off["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("Writing drafts is off")
        );
        core.update_agent_access(|access| access.can_write_drafts = true)
            .await
            .unwrap();
        send(&mut host_write, draft).await;
        let unknown_account = receive(&mut lines).await;
        assert_eq!(unknown_account["result"]["isError"], true);
        assert!(
            unknown_account["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("isn't available to agents")
        );

        // Changing mail stays off until the user allows it.
        send(
            &mut host_write,
            json!({
                "jsonrpc": "2.0", "id": 7, "method": "tools/call",
                "params": {
                    "name": "move_messages",
                    "arguments": { "message_ids": [1], "to": "trash" },
                },
            }),
        )
        .await;
        let moving_off = receive(&mut lines).await;
        assert_eq!(moving_off["result"]["isError"], true);
        assert!(
            moving_off["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("Moving messages is off")
        );
        send(
            &mut host_write,
            json!({
                "jsonrpc": "2.0", "id": 8, "method": "tools/call",
                "params": { "name": "send_draft", "arguments": { "draft_id": 1 } },
            }),
        )
        .await;
        let sending_off = receive(&mut lines).await;
        assert_eq!(sending_off["result"]["isError"], true);
        assert!(
            sending_off["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("Asking to send is off")
        );

        // Once access is off, nothing more is read or logged.
        core.update_agent_access(|access| access.enabled = false)
            .await
            .unwrap();
        send(
            &mut host_write,
            json!({
                "jsonrpc": "2.0", "id": 5, "method": "tools/call",
                "params": { "name": "list_accounts", "arguments": {} },
            }),
        )
        .await;
        let off = receive(&mut lines).await;
        assert_eq!(off["result"]["isError"], true);
        assert!(
            off["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("Agent access is off")
        );

        let activity = core.agent_activity(10).await.unwrap();
        assert_eq!(
            activity
                .iter()
                .map(|entry| (entry.agent.as_str(), entry.tool.as_str(), entry.outcome))
                .collect::<Vec<_>>(),
            [
                (
                    "test-host",
                    "send_draft",
                    flectar_mail_core::models::AgentOutcome::Refused
                ),
                (
                    "test-host",
                    "move_messages",
                    flectar_mail_core::models::AgentOutcome::Refused
                ),
                (
                    "test-host",
                    "create_draft",
                    flectar_mail_core::models::AgentOutcome::Refused
                ),
                (
                    "test-host",
                    "create_draft",
                    flectar_mail_core::models::AgentOutcome::Refused
                ),
                (
                    "test-host",
                    "list_folders",
                    flectar_mail_core::models::AgentOutcome::Refused
                ),
                (
                    "test-host",
                    "list_accounts",
                    flectar_mail_core::models::AgentOutcome::Done
                ),
            ]
        );
    }

    #[tokio::test]
    async fn without_the_app_every_call_says_how_to_reach_it() {
        let profile = tempfile::tempdir().unwrap();
        let response = forward(
            profile.path(),
            &Request {
                protocol: protocol::PROTOCOL,
                version: "1".into(),
                agent: "test-host".into(),
                tool: "list_accounts".into(),
                arguments: Default::default(),
            },
        )
        .await;
        assert!(response.is_error);
        assert_eq!(response.text, NOT_REACHABLE);
    }
}
