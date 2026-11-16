//! The narrow surface the TUI programs against, plus its two
//! implementations.
//!
//! [`LupinFacade`] exposes exactly what the screens need:
//! `status()`, `tools()`, and `call(tool, args)` returning
//! `serde_json::Value`. [`FakeFacade`] is in-memory and scripted for
//! tests. [`McpFacade`] is the real wiring: an rmcp MCP stdio client
//! that spawns the member's own `lupin` server binary, following the
//! shape of phlow's MCP client (a private tokio runtime inside a
//! synchronous surface, modern-era `server/discover` only, bounded
//! per-request waits, bounded result size).
//!
//! Lane handling lives here too: [`Lane`], [`lane_visible`], and
//! [`filter_records`] implement the client-side half of lupin's lane
//! isolation. Filtering fails closed — a record with a missing or
//! unrecognised lane is dropped, never guessed at (the same posture
//! as the server's own ledger parser).

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use serde_json::{Value, json};

/// Largest single tool result the facade will hand back, serialized.
pub const RESULT_BYTES_MAX: usize = 4 * 1024 * 1024;

/// Default per-request wait for a tool call.
pub const RESPONSE_TIMEOUT_DEFAULT: Duration = Duration::from_secs(30);

/// A revenue lane. Lupin has exactly two; this TUI views one at a
/// time and, in this build, only ever the `sfw` one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    /// The Qompass lane.
    Sfw,
    /// The NTRanch lane.
    Nsfw,
}

impl Lane {
    /// The wire spelling of this lane.
    pub fn as_str(self) -> &'static str {
        match self {
            Lane::Sfw => "sfw",
            Lane::Nsfw => "nsfw",
        }
    }

    /// Parse a wire lane spelling; anything else is not a lane.
    pub fn parse(text: &str) -> Option<Lane> {
        match text {
            "sfw" => Some(Lane::Sfw),
            "nsfw" => Some(Lane::Nsfw),
            _ => None,
        }
    }
}

impl fmt::Display for Lane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a record in `record_lane` may appear in a `view` view.
/// Fail-closed: an unparseable lane is visible in no view.
pub fn lane_visible(view: Lane, record_lane: &str) -> bool {
    Lane::parse(record_lane) == Some(view)
}

/// Split `records` into those visible in `view` and a drop count.
/// The dropped records are not returned in any form: a caller in the
/// wrong lane cannot reach them through this TUI at all.
pub fn filter_records(records: &[Value], view: Lane) -> (Vec<Value>, usize) {
    let mut visible = Vec::new();
    let mut dropped = 0usize;
    for record in records {
        let lane = record.get("lane").and_then(Value::as_str).unwrap_or("");
        if lane_visible(view, lane) {
            visible.push(record.clone());
        } else {
            dropped += 1;
        }
    }
    (visible, dropped)
}

/// Failures of the facade: transport, protocol, or the server's own
/// tool error. Expected failures are typed, never panics.
#[derive(Debug)]
pub enum FacadeError {
    /// The server process could not be spawned or the session ended.
    Transport(String),
    /// The peer violated the expected protocol/result shape.
    Protocol(String),
    /// The tool itself reported an error (`isError` content).
    Tool(String),
    /// No response arrived within the caller's bound.
    Timeout(Duration),
}

impl fmt::Display for FacadeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FacadeError::Transport(m) => write!(f, "server transport failed: {m}"),
            FacadeError::Protocol(m) => write!(f, "protocol violation: {m}"),
            FacadeError::Tool(m) => write!(f, "tool reported an error: {m}"),
            FacadeError::Timeout(d) => write!(f, "no response within {}s", d.as_secs()),
        }
    }
}

impl std::error::Error for FacadeError {}

/// The operations the TUI screens need from the lupin server.
pub trait LupinFacade {
    /// Server status: name, negotiated version, and the
    /// `pipeline_summary` payload.
    fn status(&mut self) -> Result<Value, FacadeError>;
    /// Names of the tools the server offers.
    fn tools(&mut self) -> Result<Vec<String>, FacadeError>;
    /// Call one tool with a JSON-object argument value.
    fn call(&mut self, tool: &str, args: Value) -> Result<Value, FacadeError>;
    /// Human-readable server identity for the Overview screen.
    fn server_name(&self) -> String {
        "lupin".to_string()
    }
}

/// An in-memory [`LupinFacade`] for tests: scripted results, recorded
/// calls, and scriptable failures (a dying server, malformed data).
#[derive(Debug, Default)]
pub struct FakeFacade {
    /// Payload returned for `pipeline_summary`.
    pub summary_result: Value,
    /// Payload returned for `opportunity_list`.
    pub opportunities_result: Value,
    /// Payload returned for `capability_list`.
    pub capabilities_result: Value,
    /// Payload returned for `opportunity_submit`.
    pub submit_result: Value,
    /// Tool names returned by `tools()`.
    pub tools_result: Vec<String>,
    /// Every `(tool, args)` call, in order — the plan-mode proof.
    pub calls: Vec<(String, Value)>,
    /// When set, the next call to this tool fails as a dead server.
    pub die_on_tool: Option<String>,
    /// When set, the next call returns this raw (possibly malformed)
    /// value instead of the scripted payload.
    pub malformed_next: Option<Value>,
}

impl FakeFacade {
    /// A fake seeded with a mixed-lane ledger: one `sfw` and one
    /// `nsfw` opportunity and capability, so lane-filtering tests
    /// have something real to filter. Summary counts name both
    /// lanes, as an operator-scoped server would report them.
    pub fn seeded() -> FakeFacade {
        FakeFacade {
            summary_result: json!({
                "opportunities_by_status": {"validated": 1, "aligned": 1},
                "opportunities_by_lane": {"sfw": 1, "nsfw": 1},
                "capabilities": 2,
                "parse_errors": 0
            }),
            opportunities_result: json!({
                "opportunities": [
                    {"id": "OPP-SFW-001", "title": "Test Grant", "lane": "sfw",
                     "status": "validated", "capability": "CAP-FATHOM",
                     "deadline": "2026-11-03", "next_action": "draft"},
                    {"id": "OPP-NSFW-001", "title": "Test Pack", "lane": "nsfw",
                     "status": "aligned", "capability": "CAP-STROBE-ASSETS",
                     "deadline": "rolling", "next_action": "review"}
                ],
                "parse_errors": 0
            }),
            capabilities_result: json!({
                "capabilities": [
                    {"id": "CAP-FATHOM", "lane": "sfw", "name": "fathom",
                     "revenue_form": "Product + services", "proof": "Published"},
                    {"id": "CAP-STROBE-ASSETS", "lane": "nsfw", "name": "strobe assets",
                     "revenue_form": "Product sales", "proof": "Account live"}
                ]
            }),
            submit_result: json!({"filed": "scans/SCAN-INBOX.md",
                                  "note": "candidate only"}),
            tools_result: vec![
                "capability_list".to_string(),
                "capability_match".to_string(),
                "opportunity_get".to_string(),
                "opportunity_list".to_string(),
                "opportunity_submit".to_string(),
                "pipeline_summary".to_string(),
            ],
            calls: Vec::new(),
            die_on_tool: None,
            malformed_next: None,
        }
    }
}

impl LupinFacade for FakeFacade {
    fn status(&mut self) -> Result<Value, FacadeError> {
        Ok(json!({
            "server": "lupin",
            "version": "1.0.0",
            "summary": self.call("pipeline_summary", json!({}))?,
        }))
    }

    fn tools(&mut self) -> Result<Vec<String>, FacadeError> {
        Ok(self.tools_result.clone())
    }

    fn call(&mut self, tool: &str, args: Value) -> Result<Value, FacadeError> {
        self.calls.push((tool.to_string(), args.clone()));
        if self.die_on_tool.as_deref() == Some(tool) {
            self.die_on_tool = None;
            return Err(FacadeError::Transport(
                "server died mid-call (scripted)".to_string(),
            ));
        }
        if let Some(raw) = self.malformed_next.take() {
            return Ok(raw);
        }
        match tool {
            "pipeline_summary" => Ok(self.summary_result.clone()),
            "opportunity_list" => Ok(self.opportunities_result.clone()),
            "capability_list" => Ok(self.capabilities_result.clone()),
            "opportunity_submit" => Ok(self.submit_result.clone()),
            "opportunity_get" => {
                let id = args.get("id").and_then(Value::as_str).unwrap_or("");
                let found = self
                    .opportunities_result
                    .get("opportunities")
                    .and_then(Value::as_array)
                    .and_then(|items| items.iter().find(|o| o.get("id") == Some(&json!(id))))
                    .cloned();
                match found {
                    Some(item) => Ok(item),
                    None => Err(FacadeError::Tool(
                        "opportunity not found (or outside lane scope)".to_string(),
                    )),
                }
            }
            "capability_match" => Ok(json!({"matches": [], "advisory": true})),
            other => Err(FacadeError::Tool(format!("unknown tool {other}"))),
        }
    }
}

/// The real facade: an rmcp client session with a spawned `lupin`
/// server process. Synchronous at this surface (one request in
/// flight at a time); the async client runs on a private runtime
/// inside, exactly as in phlow's MCP client.
pub struct McpFacade {
    runtime: Option<tokio::runtime::Runtime>,
    service: Option<rmcp::service::RunningService<rmcp::RoleClient, rmcp::model::ClientConfig>>,
    server_name: Option<String>,
    negotiated_version: String,
}

impl McpFacade {
    /// Spawn `command` with `args` and discover it at the modern
    /// era (2026-07-28) only. A server that cannot discover there
    /// is a negotiation failure, never a silent downgrade.
    pub fn connect(command: &str, args: &[String]) -> Result<McpFacade, FacadeError> {
        use rmcp::model::{ClientCapabilities, ClientConfig, Implementation, ProtocolVersion};
        use rmcp::service::ClientServiceExt;
        use rmcp::transport::TokioChildProcess;

        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| FacadeError::Transport(format!("client runtime: {e}")))?;
        let (transport, _stderr) = {
            let _enter = runtime.enter();
            TokioChildProcess::builder(cmd)
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|e| FacadeError::Transport(format!("{command}: {e}")))?
        };
        let handler = ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("lupin-tui", env!("CARGO_PKG_VERSION")),
        );
        let service = runtime.block_on(async {
            let connect = handler.serve_with_lifecycle(
                transport,
                rmcp::service::ClientLifecycleMode::Discover {
                    preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                },
            );
            match tokio::time::timeout(RESPONSE_TIMEOUT_DEFAULT, connect).await {
                Ok(Ok(svc)) => Ok(svc),
                Ok(Err(e)) => Err(FacadeError::Transport(format!("connect failed: {e}"))),
                Err(_) => Err(FacadeError::Timeout(RESPONSE_TIMEOUT_DEFAULT)),
            }
        })?;
        let (negotiated_version, server_name) = match service.peer().peer_info() {
            Some(info) => (
                info.protocol_version.to_string(),
                info.server_info.as_ref().map(|i| i.name.clone()),
            ),
            None => ("2026-07-28".to_string(), None),
        };
        Ok(McpFacade {
            runtime: Some(runtime),
            service: Some(service),
            server_name,
            negotiated_version,
        })
    }

    fn send(
        &mut self,
        request: rmcp::model::ClientRequest,
        timeout: Duration,
    ) -> Result<rmcp::model::ServerResult, FacadeError> {
        use rmcp::service::PeerRequestOptions;
        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| FacadeError::Transport("client session is closed".to_string()))?;
        let service = self
            .service
            .as_ref()
            .ok_or_else(|| FacadeError::Transport("client session is closed".to_string()))?;
        let peer = service.peer().clone();
        runtime.block_on(async move {
            let handle = peer
                .send_request_with_option(request, PeerRequestOptions::with_timeout(timeout))
                .await
                .map_err(|e| FacadeError::Transport(format!("server transport failed: {e}")))?;
            handle
                .await_response()
                .await
                .map_err(|e| FacadeError::Transport(format!("server transport failed: {e}")))
        })
    }
}

impl LupinFacade for McpFacade {
    fn status(&mut self) -> Result<Value, FacadeError> {
        let summary = self.call("pipeline_summary", json!({}))?;
        let tool_count = self.tools().map(|t| t.len()).unwrap_or(0);
        Ok(json!({
            "server": self.server_name.clone().unwrap_or_else(|| "lupin".to_string()),
            "version": self.negotiated_version,
            "tools": tool_count,
            "summary": summary,
        }))
    }

    fn tools(&mut self) -> Result<Vec<String>, FacadeError> {
        use rmcp::model::{ClientRequest, ListToolsRequest};
        let result = self.send(
            ClientRequest::ListToolsRequest(ListToolsRequest {
                method: Default::default(),
                params: None,
                extensions: Default::default(),
            }),
            RESPONSE_TIMEOUT_DEFAULT,
        )?;
        let rmcp::model::ServerResult::ListToolsResult(result) = result else {
            return Err(FacadeError::Protocol(
                "tools/list answered with an unexpected result type".to_string(),
            ));
        };
        Ok(result
            .tools
            .iter()
            .map(|t| t.name.clone().into_owned())
            .collect())
    }

    fn call(&mut self, tool: &str, args: Value) -> Result<Value, FacadeError> {
        use rmcp::model::{CallToolRequest, CallToolRequestParams, ClientRequest};
        let arguments = match args {
            Value::Object(map) => Some(map),
            Value::Null => None,
            _ => {
                return Err(FacadeError::Protocol(
                    "tool arguments must be a JSON object".to_string(),
                ));
            }
        };
        let mut params = CallToolRequestParams::new(tool.to_string());
        if let Some(arguments) = arguments {
            params = params.with_arguments(arguments);
        }
        let result = self.send(
            ClientRequest::CallToolRequest(CallToolRequest::new(params)),
            RESPONSE_TIMEOUT_DEFAULT,
        )?;
        let rmcp::model::ServerResult::CallToolResult(result) = result else {
            return Err(FacadeError::Protocol(format!(
                "tool '{tool}' answered with an unexpected result type"
            )));
        };
        let text = result
            .content
            .first()
            .and_then(|item| item.as_text())
            .map(|t| t.text.as_str())
            .ok_or_else(|| {
                FacadeError::Protocol(format!("tool '{tool}' result has no text content"))
            })?;
        if result.is_error.unwrap_or(false) {
            return Err(FacadeError::Tool(text.to_string()));
        }
        let value: Value = serde_json::from_str(text).unwrap_or_else(|_| json!({"text": text}));
        let size = serde_json::to_string(&value).map(|s| s.len()).unwrap_or(0);
        if size > RESULT_BYTES_MAX {
            return Err(FacadeError::Protocol(format!(
                "result exceeds the {RESULT_BYTES_MAX}-byte bound"
            )));
        }
        Ok(value)
    }

    fn server_name(&self) -> String {
        self.server_name
            .clone()
            .unwrap_or_else(|| "lupin".to_string())
    }
}

impl Drop for McpFacade {
    fn drop(&mut self) {
        if let (Some(runtime), Some(mut service)) = (self.runtime.take(), self.service.take()) {
            runtime.block_on(async move {
                let _ = service.close().await;
            });
        }
    }
}

/// Ordered map helper for deterministic test fixtures.
pub fn ordered_map(pairs: &[(&str, Value)]) -> Value {
    let mut map = BTreeMap::new();
    for (k, v) in pairs {
        map.insert((*k).to_string(), v.clone());
    }
    Value::Object(map.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_lane_parse_and_visibility() {
        assert_eq!(Lane::parse("sfw"), Some(Lane::Sfw));
        assert_eq!(Lane::parse("nsfw"), Some(Lane::Nsfw));
        assert!(lane_visible(Lane::Sfw, "sfw"));
        assert!(!lane_visible(Lane::Sfw, "nsfw"));
    }

    #[test]
    fn adversarial_unknown_lane_is_visible_nowhere() {
        assert!(!lane_visible(Lane::Sfw, ""));
        assert!(!lane_visible(Lane::Sfw, "operator"));
        assert!(!lane_visible(Lane::Nsfw, "SFw"));
        assert_eq!(Lane::parse("bogus"), None);
    }

    #[test]
    fn validation_filter_keeps_only_view_lane() {
        let records = vec![
            json!({"id": "A", "lane": "sfw"}),
            json!({"id": "B", "lane": "nsfw"}),
            json!({"id": "C", "lane": "sfw"}),
        ];
        let (visible, dropped) = filter_records(&records, Lane::Sfw);
        assert_eq!(visible.len(), 2);
        assert_eq!(dropped, 1);
        assert!(visible.iter().all(|r| r["lane"] == "sfw"));
    }

    #[test]
    fn adversarial_filter_drops_missing_lane_fail_closed() {
        let records = vec![json!({"id": "X"}), json!({"id": "Y", "lane": 42})];
        let (visible, dropped) = filter_records(&records, Lane::Sfw);
        assert!(visible.is_empty());
        assert_eq!(dropped, 2);
    }

    #[test]
    fn validation_fake_records_calls_and_answers() {
        let mut fake = FakeFacade::seeded();
        let value = fake.call("opportunity_list", json!({})).unwrap();
        assert_eq!(value["opportunities"].as_array().unwrap().len(), 2);
        assert_eq!(fake.calls.len(), 1);
        assert_eq!(fake.calls[0].0, "opportunity_list");
    }

    #[test]
    fn adversarial_fake_server_death_is_typed_error() {
        let mut fake = FakeFacade::seeded();
        fake.die_on_tool = Some("pipeline_summary".to_string());
        match fake.call("pipeline_summary", json!({})) {
            Err(FacadeError::Transport(_)) => {}
            other => panic!("expected transport error, got {other:?}"),
        }
        // The failure is one-shot: the next call answers again.
        assert!(fake.call("pipeline_summary", json!({})).is_ok());
    }
}
