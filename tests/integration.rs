//! Integration tests: drive the real binary over stdio JSON-RPC against
//! fixture engine files in a temp root. Half validation, half adversarial.

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
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn call(id: i64, tool: &str, args: serde_json::Value) -> String {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": tool, "arguments": args}})
    .to_string()
}

fn result_text(resp: &serde_json::Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

// ---------- validation ----------

#[test]
fn initialize_and_list_round_trip() {
    let root = fixture_root("init");
    let resps = rpc(
        &root,
        "operator",
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        ],
    );
    assert_eq!(resps.len(), 2);
    assert_eq!(resps[0]["result"]["serverInfo"]["name"], "lupin");
    assert_eq!(resps[1]["result"]["tools"].as_array().unwrap().len(), 6);
}

#[test]
fn list_returns_both_lanes_for_operator() {
    let root = fixture_root("list");
    let req = call(1, "opportunity_list", serde_json::json!({}));
    let resps = rpc(&root, "operator", &[&req]);
    let text = result_text(&resps[0]);
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
    let resps = rpc(&root, "operator", &[&req]);
    assert!(result_text(&resps[0]).contains("filed"));
    let inbox = std::fs::read_to_string(root.join("scans/SCAN-INBOX.md")).unwrap();
    assert!(inbox.contains("New Grant"));
}

// ---------- adversarial ----------

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
    let resps = rpc(&root, "sfw", &[&list_req, &get_req, &cap_req]);
    assert!(!result_text(&resps[0]).contains("NSFW"));
    assert!(result_text(&resps[1]).contains("error")); // get refuses outside scope
    assert!(!result_text(&resps[2]).contains("STROBE"));
    assert!(result_text(&resps[2]).contains("FATHOM"));
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
    let resps = rpc(&root, "sfw", &[&req]);
    assert!(result_text(&resps[0]).contains("error"));
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
    let _ = rpc(&root, "operator", &[&req]);
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
    let resps = rpc(&root, "operator", &[&bad_tool, &bad_id]);
    assert_eq!(resps[0]["result"]["isError"], true);
    assert_eq!(resps[1]["result"]["isError"], true);
}
