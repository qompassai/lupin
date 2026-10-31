//! The rmcp server: tool/resource surface and handler dispatch.
//!
//! Domain logic does not move in the WS2 migration — the six tool
//! handlers and the three resource reads below are the hand-rolled
//! server's, verbatim in behavior (same argument handling — missing
//! or non-string fields degrade to empty, unknown arguments are
//! ignored — same payload construction, same failure taxonomy):
//! domain failures return as `isError` content carrying
//! `{"error": msg}`; an unknown resource URI is the one protocol
//! error (`-32602`). Only the framing changes sides of the seam:
//! rmcp owns the envelope, the era guard (see `guard.rs`) owns the
//! stdio front door, and this type owns the surface.
//!
//! Modern-only posture lives in two places: the era guard refuses
//! `initialize` before the SDK sees it, and
//! [`ServerHandler::supported_protocol_versions`] pins this handler
//! to 2026-07-28, so `server/discover` advertises exactly one
//! version and the SDK validates every request's `_meta` against it.

use std::borrow::Cow;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    InitializeResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
    ProtocolVersion, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
    ResourceContents, ServerCapabilities, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::{Value, json};

use crate::ledger::{Ledger, capability_match, lane_visible, load, summary};

/// Server configuration (the baseline's CLI surface, unchanged):
/// the engine root plus the caller's lane scope.
pub struct Config {
    pub root: PathBuf,
    pub scope: String, // "operator" | "sfw"
}

/// The tool catalog: names, descriptions, and input schemas are the
/// baseline's, byte-for-byte — rmcp accepts raw schemas, so no
/// schemars translation stands between the baseline and the wire.
fn tool_definitions() -> Vec<Tool> {
    let specs: Vec<(&str, &str, Value)> = vec![
        (
            "opportunity_list",
            "List ledger opportunities (optionally filtered by status). Lane scope enforced.",
            json!({"type": "object", "properties": {"status": {"type": "string"}}}),
        ),
        (
            "opportunity_get",
            "Get one ledger entry by id, including its raw block.",
            json!({"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}),
        ),
        (
            "opportunity_submit",
            "File a CANDIDATE into the scan inbox. Never writes the ledger; Pax validates.",
            json!({"type": "object", "properties": {
                "title": {"type": "string"}, "source": {"type": "string"},
                "lane": {"type": "string"}, "why_it_fits": {"type": "string"}},
                "required": ["title", "source", "lane", "why_it_fits"]}),
        ),
        (
            "capability_list",
            "List the capability registry (the alignment vocabulary). Lane scope enforced.",
            json!({"type": "object", "properties": {}}),
        ),
        (
            "capability_match",
            "Advisory keyword match of free text against capabilities. Never auto-aligns.",
            json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}),
        ),
        (
            "pipeline_summary",
            "Counts by status/lane plus parse health — the value-demonstration view.",
            json!({"type": "object", "properties": {}}),
        ),
    ];
    specs
        .into_iter()
        .map(|(name, description, schema)| {
            let Value::Object(map) = schema else {
                unreachable!("catalog schemas are objects");
            };
            Tool::new(name, description, map)
        })
        .collect()
}

/// The resource catalog (the baseline's, in order).
fn resource_definitions() -> Vec<Resource> {
    [
        ("revenue://ledger", "Opportunity ledger"),
        ("revenue://capabilities", "Capability registry"),
        ("revenue://design", "Engine design"),
    ]
    .into_iter()
    .map(|(uri, name)| Resource::new(uri, name).with_mime_type("text/markdown"))
    .collect()
}

fn text_result(value: &Value, is_error: bool) -> CallToolResult {
    let content = vec![ContentBlock::text(value.to_string())];
    if is_error {
        CallToolResult::error(content)
    } else {
        CallToolResult::success(content)
    }
}

fn err_result(msg: &str) -> CallToolResult {
    text_result(&json!({"error": msg}), true)
}

/// The baseline's dispatch, unchanged: every call reloads the
/// ledger from disk, lane scope is enforced per entry, and the one
/// write files a candidate into scans/ — never the ledger.
fn call_tool(cfg: &Config, name: &str, args: &Value) -> CallToolResult {
    let data: Ledger = load(&cfg.root);
    match name {
        "opportunity_list" => {
            let status = args.get("status").and_then(|v| v.as_str());
            let items: Vec<Value> = data
                .opportunities
                .iter()
                .filter(|o| lane_visible(&cfg.scope, &o.lane))
                .filter(|o| status.map(|s| o.status == s).unwrap_or(true))
                .map(|o| {
                    json!({"id": o.id, "title": o.title, "lane": o.lane,
                           "status": o.status, "capability": o.capability,
                           "deadline": o.deadline, "next_action": o.next_action})
                })
                .collect();
            text_result(
                &json!({"opportunities": items, "parse_errors": data.parse_errors}),
                false,
            )
        }
        "opportunity_get" => {
            let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
            match data
                .opportunities
                .iter()
                .find(|o| o.id == id && lane_visible(&cfg.scope, &o.lane))
            {
                Some(o) => text_result(
                    &json!({"id": o.id, "title": o.title, "lane": o.lane,
                            "status": o.status, "capability": o.capability,
                            "pays": o.pays, "deadline": o.deadline,
                            "next_action": o.next_action, "raw": o.raw}),
                    false,
                ),
                None => err_result("opportunity not found (or outside lane scope)"),
            }
        }
        "opportunity_submit" => {
            let lane = args.get("lane").and_then(|v| v.as_str()).unwrap_or("");
            if lane != "sfw" && lane != "nsfw" {
                return err_result("lane must be 'sfw' or 'nsfw'");
            }
            if !lane_visible(&cfg.scope, lane) {
                return err_result("lane outside caller scope; candidate refused");
            }
            let title = args.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let source = args.get("source").and_then(|v| v.as_str()).unwrap_or("");
            let why = args
                .get("why_it_fits")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if title.is_empty() || source.is_empty() || why.is_empty() {
                return err_result("title, source, and why_it_fits are all required");
            }
            let scans = cfg.root.join("scans");
            if fs::create_dir_all(&scans).is_err() {
                return err_result("cannot create scans directory");
            }
            let inbox = scans.join("SCAN-INBOX.md");
            let entry = format!(
                "\n### CANDIDATE — {title}\n- **Lane:** {lane}\n- **Source:** {source}\n- **Why it fits:** {why}\n- **Status:** candidate (unvalidated — filed by MCP caller)\n"
            );
            match OpenOptions::new().create(true).append(true).open(&inbox) {
                Ok(mut f) => match f.write_all(entry.as_bytes()) {
                    Ok(()) => text_result(
                        &json!({"filed": inbox.display().to_string(),
                                "note": "candidate only; Pax aligns + validates before any ledger entry"}),
                        false,
                    ),
                    Err(e) => err_result(&format!("inbox write failed: {e}")),
                },
                Err(e) => err_result(&format!("inbox open failed: {e}")),
            }
        }
        "capability_list" => {
            let items: Vec<Value> = data
                .capabilities
                .iter()
                .filter(|c| lane_visible(&cfg.scope, &c.lane))
                .map(|c| {
                    json!({"id": c.id, "lane": c.lane, "name": c.name,
                           "revenue_form": c.revenue_form, "proof": c.proof})
                })
                .collect();
            text_result(&json!({"capabilities": items}), false)
        }
        "capability_match" => {
            let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let ranked = capability_match(&data, &cfg.scope, text);
            let items: Vec<Value> = ranked
                .iter()
                .map(|(id, score)| json!({"capability": id, "score": score}))
                .collect();
            text_result(&json!({"matches": items, "advisory": true}), false)
        }
        "pipeline_summary" => text_result(&summary(&data, &cfg.scope), false),
        _ => err_result("unknown tool"),
    }
}

/// The baseline's resource read, unchanged: an unknown URI is the
/// surface's one protocol error; a missing file reads as empty.
fn read_resource(cfg: &Config, uri: &str) -> Result<ReadResourceResult, ErrorData> {
    let file = match uri {
        "revenue://ledger" => "OPPORTUNITIES.md",
        "revenue://capabilities" => "CAPABILITIES.md",
        "revenue://design" => "DESIGN.md",
        _ => {
            return Err(ErrorData::invalid_params("unknown resource uri", None));
        }
    };
    let text = fs::read_to_string(cfg.root.join(file)).unwrap_or_default();
    Ok(ReadResourceResult::new(vec![
        ResourceContents::TextResourceContents {
            uri: uri.to_string(),
            mime_type: Some("text/markdown".to_string()),
            text,
            meta: None,
        },
    ]))
}

/// The server handler. The baseline served sequentially and every
/// call reloads state from disk, so dispatch is stateless; it runs
/// on a blocking thread so file IO cannot stall the runtime.
pub struct LupinServer {
    cfg: Arc<Config>,
}

impl LupinServer {
    pub fn new(cfg: Config) -> Self {
        LupinServer { cfg: Arc::new(cfg) }
    }
}

impl ServerHandler for LupinServer {
    fn get_info(&self) -> InitializeResult {
        // Both model types are non-exhaustive: start from the
        // default and set exactly the fields this server reports.
        // The baseline reported no instructions; none are set.
        let mut server_info = Implementation::default();
        server_info.name = "lupin".to_string();
        server_info.version = env!("CARGO_PKG_VERSION").to_string();
        let mut info = InitializeResult::default();
        info.protocol_version = ProtocolVersion::V_2026_07_28;
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .build();
        info.server_info = server_info;
        info
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(&[ProtocolVersion::V_2026_07_28])
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: tool_definitions(),
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let name = request.name.to_string();
        let arguments = Value::Object(request.arguments.unwrap_or_default());
        let cfg = Arc::clone(&self.cfg);
        let outcome = tokio::task::spawn_blocking(move || call_tool(&cfg, &name, &arguments)).await;
        match outcome {
            Ok(result) => Ok(result.into()),
            Err(join) => Err(ErrorData::internal_error(
                format!("tool task failed: {join}"),
                None,
            )),
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult {
            resources: resource_definitions(),
            ..Default::default()
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let cfg = Arc::clone(&self.cfg);
        let outcome = tokio::task::spawn_blocking(move || read_resource(&cfg, &request.uri)).await;
        match outcome {
            Ok(result) => result.map(ReadResourceResponse::from),
            Err(join) => Err(ErrorData::internal_error(
                format!("resource task failed: {join}"),
                None,
            )),
        }
    }
}
