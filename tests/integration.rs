//! Integration tests: drive the real binary over stdio JSON-RPC against
//! fixture engine files in a temp root. Half validation, half adversarial.
//!
//! WS2 (D-113): the wire is the modern era only — sessions open with
//! `server/discover`, every request carries `_meta`, and legacy
//! `initialize` contact is refused with the typed `-32022`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const LEDGER: &str = "\
### OPP-SFW-001 — Test Grant
- **Lane:** sfw · **Capability:** CAP-FATHOM
- **Pays:** €5,000 (as stated)
- **Deadline:** 2026-11-03
- **Status:** validated
- **Next action:** Pax drafts.

### OPP-NSFW-001 — Test Pack
- **Lane:** nsfw · **Capability:** CAP-STROBE-ASSETS
- **Pays:** unstated
- **Deadline:** rolling
- **Status:** aligned
- **Next action:** Pax reviews.
";

const CAPS: &str = "\
## SFW lane — Qompass

| ID | Capability | Revenue form | Proof / state |
|----|-----------|--------------|----------------|
| CAP-FATHOM | fathom — Rust MCP reverse-engineering server | Product + services | Published |

## NSFW lane — NTRanch

| ID | Capability | Revenue form | Proof / state |
|----|-----------|--------------|----------------|
| CAP-STROBE-ASSETS | Strobe digital products | Product sales | Account live |
";

const META: &str = r#""_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientInfo": {"name": "lupin-test", "version": "0"}, "io.modelcontextprotocol/clientCapabilities": {}}"#;

fn fixture_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rev-align-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("scans")).unwrap();
    std::fs::write(dir.join("OPPORTUNITIES.md"), LEDGER).unwrap();
    std::fs::write(dir.join("CAPABILITIES.md"), CAPS).unwrap();
    std::fs::write(dir.join("DESIGN.md"), "# design\n").unwrap();
    dir
}

/// Send newline-delimited JSON-RPC, collect every response line.
fn rpc(root: &Path, scope: &str, requests: &[&str]) -> Vec<serde_json::Value> {
    let bin = env!("CARGO_BIN_EXE_lupin");
    let mut child = Command::new(bin)
        .args(["--root", root.to_str().unwrap(), "--lane-scope", scope])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        for req in requests {
            writeln!(stdin, "{req}").unwrap();
        }
    } // close stdin -> server exits on EOF
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "server exit: {:?}", out.status);
    // rmcp dispatches pipelined requests concurrently, so responses
    // may arrive out of request order (legal JSON-RPC; the baseline
    // answered strictly sequentially). Tests correlate by id: sort
    // the parsed responses before indexing them positionally.
    let mut resps: Vec<serde_json::Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    resps.sort_by_key(|r| r["id"].as_i64().unwrap_or(i64::MAX));
    resps
}

fn discover() -> String {
    format!(r#"{{"jsonrpc":"2.0","id":0,"method":"server/discover","params":{{{META}}}}}"#)
}

fn call(id: i64, tool: &str, args: serde_json::Value) -> String {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": tool, "arguments": args,
                   "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28",
                             "io.modelcontextprotocol/clientInfo": {"name": "lupin-test", "version": "0"},
                             "io.modelcontextprotocol/clientCapabilities": {}}}})
    .to_string()
}

fn request(id: i64, method: &str, params: serde_json::Value) -> String {
    let mut params = params;
    params["_meta"] = serde_json::json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "lupin-test", "version": "0"},
        "io.modelcontextprotocol/clientCapabilities": {}});
    serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

fn result_text(resp: &serde_json::Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

// ---------- validation ----------

#[test]
fn discover_and_list_round_trip() {
    let root = fixture_root("init");
    let list = request(2, "tools/list", serde_json::json!({}));
    let resps = rpc(&root, "operator", &[&discover(), &list]);
    assert_eq!(resps.len(), 2);
    assert_eq!(
        resps[0]["result"]["supportedVersions"],
        serde_json::json!(["2026-07-28"])
    );
    assert_eq!(
        resps[0]["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "lupin"
    );
    assert_eq!(resps[1]["result"]["tools"].as_array().unwrap().len(), 6);
}

#[test]
fn resources_round_trip() {
    let root = fixture_root("resources");
    let list = request(2, "resources/list", serde_json::json!({}));
    let read = request(
        3,
        "resources/read",
        serde_json::json!({"uri": "revenue://ledger"}),
    );
    let resps = rpc(&root, "operator", &[&discover(), &list, &read]);
    let resources = resps[1]["result"]["resources"].as_array().unwrap();
    assert_eq!(resources.len(), 3);
    assert_eq!(resources[0]["uri"], "revenue://ledger");
    assert_eq!(resources[0]["mimeType"], "text/markdown");
    let contents = resps[2]["result"]["contents"].as_array().unwrap();
    assert_eq!(contents[0]["uri"], "revenue://ledger");
    assert!(
        contents[0]["text"]
            .as_str()
            .unwrap()
            .contains("OPP-SFW-001")
    );
}

#[test]
fn list_returns_both_lanes_for_operator() {
    let root = fixture_root("list");
    let req = call(1, "opportunity_list", serde_json::json!({}));
    let resps = rpc(&root, "operator", &[&discover(), &req]);
    let text = result_text(&resps[1]);
    assert!(text.contains("OPP-SFW-001"));
    assert!(text.contains("OPP-NSFW-001"));
}

#[test]
fn submit_files_candidate_to_inbox() {
    let root = fixture_root("submit");
    let req = call(
        1,
        "opportunity_submit",
        serde_json::json!({"title": "New Grant", "source": "https://example.org/call",
            "lane": "sfw", "why_it_fits": "matches CAP-FATHOM"}),
    );
    let resps = rpc(&root, "operator", &[&discover(), &req]);
    assert!(result_text(&resps[1]).contains("filed"));
    let inbox = std::fs::read_to_string(root.join("scans/SCAN-INBOX.md")).unwrap();
    assert!(inbox.contains("New Grant"));
}

// ---------- adversarial ----------

#[test]
fn legacy_contact_is_refused_and_the_server_survives() {
    let root = fixture_root("refusal");
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#;
    let no_meta = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
    let summary = call(3, "pipeline_summary", serde_json::json!({}));
    let resps = rpc(&root, "operator", &[init, no_meta, &discover(), &summary]);
    // rpc() sorts by id: [discover(0), init(1), no-meta(2), summary(3)].
    assert_eq!(resps[1]["error"]["code"], -32022);
    assert_eq!(resps[1]["error"]["data"]["requested"], "2024-11-05");
    assert_eq!(
        resps[1]["error"]["data"]["supported"],
        serde_json::json!(["2026-07-28"])
    );
    // A tools/list with no _meta, before any discover: refused by the
    // SDK's metadata validation, not served.
    assert!(resps[2]["error"].is_object(), "no-meta response: {resps:?}");
    // After the refusals the server still serves modern frames.
    assert!(result_text(&resps[3]).contains("opportunities_by_status"));
}

#[test]
fn sfw_scope_hides_nsfw_everywhere() {
    let root = fixture_root("scope");
    let list_req = call(1, "opportunity_list", serde_json::json!({}));
    let get_req = call(
        2,
        "opportunity_get",
        serde_json::json!({"id": "OPP-NSFW-001"}),
    );
    let cap_req = call(3, "capability_list", serde_json::json!({}));
    let resps = rpc(&root, "sfw", &[&discover(), &list_req, &get_req, &cap_req]);
    assert!(!result_text(&resps[1]).contains("NSFW"));
    assert!(result_text(&resps[2]).contains("error")); // get refuses outside scope
    assert!(!result_text(&resps[3]).contains("STROBE"));
    assert!(result_text(&resps[3]).contains("FATHOM"));
}

#[test]
fn submit_cannot_cross_lane_scope() {
    let root = fixture_root("crosslane");
    let req = call(
        1,
        "opportunity_submit",
        serde_json::json!({"title": "Sneaky", "source": "https://example.org",
            "lane": "nsfw", "why_it_fits": "n/a"}),
    );
    let resps = rpc(&root, "sfw", &[&discover(), &req]);
    assert!(result_text(&resps[1]).contains("error"));
    assert!(!root.join("scans/SCAN-INBOX.md").exists());
}

#[test]
fn submit_never_modifies_the_ledger() {
    let root = fixture_root("ledger-immutable");
    let before = std::fs::read(root.join("OPPORTUNITIES.md")).unwrap();
    let req = call(
        1,
        "opportunity_submit",
        serde_json::json!({"title": "Another", "source": "https://example.org",
            "lane": "nsfw", "why_it_fits": "matches CAP-STROBE-ASSETS"}),
    );
    let _ = rpc(&root, "operator", &[&discover(), &req]);
    let after = std::fs::read(root.join("OPPORTUNITIES.md")).unwrap();
    assert_eq!(before, after);
}

#[test]
fn unknown_tool_and_missing_id_error_cleanly() {
    let root = fixture_root("errors");
    let bad_tool = call(1, "opportunity_delete_everything", serde_json::json!({}));
    let bad_id = call(
        2,
        "opportunity_get",
        serde_json::json!({"id": "OPP-XXX-999"}),
    );
    let resps = rpc(&root, "operator", &[&discover(), &bad_tool, &bad_id]);
    assert_eq!(resps[1]["result"]["isError"], true);
    assert_eq!(resps[2]["result"]["isError"], true);
}

#[test]
fn adversarial_frames_keep_framing_contract() {
    let root = fixture_root("adversarial");
    let oversize = "x".repeat(5 * 1024 * 1024);
    let summary = call(9, "pipeline_summary", serde_json::json!({}));
    let resps = rpc(
        &root,
        "operator",
        &[
            &discover(),
            "this is not json",
            "[1,2,3]",
            &oversize,
            &summary,
        ],
    );
    // rpc() sorts by id with null-id framing errors last (stable, so
    // they keep arrival order among themselves):
    // [discover(0), summary(9), parse-error, -32600, oversize-error].
    assert_eq!(resps[2]["error"]["code"], -32700);
    assert_eq!(resps[2]["error"]["message"], "parse error");
    assert_eq!(resps[3]["error"]["code"], -32600);
    assert_eq!(resps[4]["error"]["code"], -32700);
    assert_eq!(
        resps[4]["error"]["message"],
        "request line exceeds size bound"
    );
    assert!(result_text(&resps[1]).contains("opportunities_by_status"));
}

#[test]
fn notification_shaped_call_is_not_executed() {
    let root = fixture_root("notify");
    let notif = r#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"opportunity_submit","arguments":{"title":"Ghost","source":"https://example.org","lane":"sfw","why_it_fits":"x"}}}"#;
    let summary = call(2, "pipeline_summary", serde_json::json!({}));
    let resps = rpc(&root, "operator", &[&discover(), notif, &summary]);
    // Only discover + the summary answer; the notification produced
    // no response and — unlike the hand-rolled baseline — no inbox
    // entry either.
    assert_eq!(resps.len(), 2);
    assert!(!root.join("scans/SCAN-INBOX.md").exists());
}

#[test]
fn unknown_resource_uri_is_invalid_params() {
    let root = fixture_root("baduri");
    let read = request(
        2,
        "resources/read",
        serde_json::json!({"uri": "revenue://bogus"}),
    );
    let resps = rpc(&root, "operator", &[&discover(), &read]);
    assert_eq!(resps[1]["error"]["code"], -32602);
    assert_eq!(resps[1]["error"]["message"], "unknown resource uri");
}
