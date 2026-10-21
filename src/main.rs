//! lupin — MCP server for the Revenue Alignment Engine (stdio, JSON-RPC).
//!
//! Serves the Revenue Alignment Engine's ledger and capability registry.
//! Posture (MCP-SPEC.md): loopback/operator use;
//! lane-scoped (an `sfw`-scoped caller can never see `nsfw` entries);
//! read-mostly — the only write files a candidate into scans/, never the
//! validated ledger.

mod ledger;

use ledger::{capability_match, lane_visible, load, summary, Ledger};
use serde_json::{json, Value};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;

struct Config {
    root: PathBuf,
    scope: String, // "operator" | "sfw"
}

fn tool_schemas() -> Value {
    json!({"tools": [
        {"name": "opportunity_list",
         "description": "List ledger opportunities (optionally filtered by status). Lane scope enforced.",
         "inputSchema": {"type": "object", "properties": {"status": {"type": "string"}}}},
        {"name": "opportunity_get",
         "description": "Get one ledger entry by id, including its raw block.",
         "inputSchema": {"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}},
        {"name": "opportunity_submit",
         "description": "File a CANDIDATE into the scan inbox. Never writes the ledger; Pax validates.",
         "inputSchema": {"type": "object", "properties": {
             "title": {"type": "string"}, "source": {"type": "string"},
             "lane": {"type": "string"}, "why_it_fits": {"type": "string"}},
             "required": ["title", "source", "lane", "why_it_fits"]}},
        {"name": "capability_list",
         "description": "List the capability registry (the alignment vocabulary). Lane scope enforced.",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "capability_match",
         "description": "Advisory keyword match of free text against capabilities. Never auto-aligns.",
         "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}},
        {"name": "pipeline_summary",
         "description": "Counts by status/lane plus parse health — the value-demonstration view.",
         "inputSchema": {"type": "object", "properties": {}}}
    ]})
}

fn text_result(value: &Value, is_error: bool) -> Value {
    json!({
        "content": [{"type": "text", "text": value.to_string()}],
        "isError": is_error
    })
}

fn err_result(msg: &str) -> Value {
    text_result(&json!({"error": msg}), true)
}

fn call_tool(cfg: &Config, name: &str, args: &Value) -> Value {
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

fn handle(cfg: &Config, req: &Value) -> Option<Value> {
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}, "resources": {}},
            "serverInfo": {"name": "lupin", "version": "0.1.0"}
        }),
        "ping" => json!({}),
        "tools/list" => tool_schemas(),
        "tools/call" => {
            let params = req.get("params").cloned().unwrap_or(json!({}));
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            call_tool(cfg, name, &args)
        }
        "resources/list" => json!({"resources": [
            {"uri": "revenue://ledger", "name": "Opportunity ledger", "mimeType": "text/markdown"},
            {"uri": "revenue://capabilities", "name": "Capability registry", "mimeType": "text/markdown"},
            {"uri": "revenue://design", "name": "Engine design", "mimeType": "text/markdown"}
        ]}),
        "resources/read" => {
            let uri = req
                .get("params")
                .and_then(|p| p.get("uri"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let file = match uri {
                "revenue://ledger" => "OPPORTUNITIES.md",
                "revenue://capabilities" => "CAPABILITIES.md",
                "revenue://design" => "DESIGN.md",
                _ => {
                    return Some(json!({"jsonrpc": "2.0", "id": id,
                        "error": {"code": -32602, "message": "unknown resource uri"}}))
                }
            };
            let text = fs::read_to_string(cfg.root.join(file)).unwrap_or_default();
            json!({"contents": [{"uri": uri, "mimeType": "text/markdown", "text": text}]})
        }
        _ => {
            if id.is_null() {
                return None; // notifications get no response
            }
            return Some(json!({"jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": "method not found"}}));
        }
    };
    if id.is_null() {
        return None;
    }
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn main() {
    let mut root: Option<PathBuf> = env::var_os("REVENUE_ROOT").map(PathBuf::from);
    let mut scope = "operator".to_string();
    let mut argv = env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--root" => root = argv.next().map(PathBuf::from),
            "--lane-scope" => {
                if let Some(v) = argv.next() {
                    scope = v;
                }
            }
            other => eprintln!("lupin: ignoring unknown arg {other}"),
        }
    }
    if scope != "operator" && scope != "sfw" {
        eprintln!("lupin: --lane-scope must be 'operator' or 'sfw'");
        std::process::exit(2);
    }
    let root = root.unwrap_or_else(|| {
        let home = env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        home.join("workspace").join("revenue-alignment-20261019")
    });
    let cfg = Config { root, scope };

    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let response = match serde_json::from_str::<Value>(trimmed) {
                    Ok(req) => handle(&cfg, &req),
                    Err(_) => Some(json!({"jsonrpc": "2.0", "id": Value::Null,
                        "error": {"code": -32700, "message": "parse error"}})),
                };
                if let Some(resp) = response {
                    let _ = writeln!(out, "{resp}");
                    let _ = out.flush();
                }
            }
            Err(_) => break,
        }
    }
    let _ = Read::read(&mut reader, &mut []);
}
