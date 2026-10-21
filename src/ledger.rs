//! Ledger and capability-registry parsing for the Revenue Alignment Engine.
//!
//! The markdown files in the engine root are the source of truth. Parsing
//! is fail-closed: an entry that lacks a lane or a status is skipped and
//! counted, never guessed at.

use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Opportunity {
    pub id: String,
    pub title: String,
    pub lane: String,
    pub status: String,
    pub capability: String,
    pub pays: String,
    pub deadline: String,
    pub next_action: String,
    pub raw: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Capability {
    pub id: String,
    pub lane: String,
    pub name: String,
    pub revenue_form: String,
    pub proof: String,
}

#[derive(Clone, Debug, Default)]
pub struct Ledger {
    pub opportunities: Vec<Opportunity>,
    pub capabilities: Vec<Capability>,
    pub parse_errors: usize,
}

/// Extract the value following `**Label:**` within a block, up to the next
/// `**` field marker, a `·` separator, or end of line — whichever is first.
fn field(block: &str, label: &str) -> String {
    let needle = format!("**{label}:**");
    let Some(start) = block.find(&needle) else {
        return String::new();
    };
    let rest = &block[start + needle.len()..];
    // A value may itself be bold-wrapped ("**2026-11-03** ← note"): then the
    // value is that bold span, not the empty string before its opener.
    let lead = rest.len() - rest.trim_start().len();
    if rest[lead..].starts_with("**") {
        let inner = &rest[lead + 2..];
        if let Some(close) = inner.find("**") {
            return inner[..close].trim().to_string();
        }
    }
    let end = rest
        .find("**")
        .unwrap_or(rest.len())
        .min(rest.find('\n').unwrap_or(rest.len()));
    rest[..end]
        .replace("**", "")
        .trim()
        .trim_end_matches('·')
        .trim()
        .to_string()
}

fn parse_opportunities(text: &str, errors: &mut usize) -> Vec<Opportunity> {
    let mut out = Vec::new();
    // Entry blocks begin at "### " headers naming an OPP id.
    let mut current: Option<(String, String)> = None; // (header, block)
    let mut blocks: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if line.starts_with("### ") {
            if let Some(done) = current.take() {
                blocks.push(done);
            }
            current = Some((
                line.trim_start_matches('#').trim().to_string(),
                String::new(),
            ));
        } else if let Some((_, block)) = &mut current {
            if line.starts_with("## ") {
                blocks.push(current.take().unwrap());
            } else {
                block.push_str(line);
                block.push('\n');
            }
        }
    }
    if let Some(done) = current.take() {
        blocks.push(done);
    }
    for (header, block) in blocks {
        // Header shape: "OPP-SFW-001 — NLnet Open Internet Stack grant"
        let mut parts = header.splitn(2, " — ");
        let id = parts.next().unwrap_or("").trim().to_string();
        let title = parts.next().unwrap_or("").trim().to_string();
        if !id.starts_with("OPP-") {
            continue;
        }
        let lane = field(&block, "Lane");
        let status = field(&block, "Status");
        if lane.is_empty() || status.is_empty() {
            *errors += 1;
            continue; // fail closed: never guess a lane or a status
        }
        // Lane bullet is "sfw · **Capability:** ..." — keep just the token.
        let lane = lane.split_whitespace().next().unwrap_or("").to_lowercase();
        out.push(Opportunity {
            id,
            title,
            lane,
            status: status.split('(').next().unwrap_or("").trim().to_lowercase(),
            capability: field(&block, "Capability"),
            pays: field(&block, "Pays"),
            deadline: field(&block, "Deadline"),
            next_action: field(&block, "Next action"),
            raw: block.trim().to_string(),
        });
    }
    out
}

fn parse_capabilities(text: &str, errors: &mut usize) -> Vec<Capability> {
    let _ = errors;
    let mut out = Vec::new();
    let mut lane = String::new();
    for line in text.lines() {
        if line.starts_with("## SFW lane") {
            lane = "sfw".to_string();
        } else if line.starts_with("## NSFW lane") {
            lane = "nsfw".to_string();
        }
        let trimmed = line.trim();
        if !trimmed.starts_with("| CAP-") {
            continue;
        }
        let cells: Vec<&str> = trimmed
            .trim_matches('|')
            .split('|')
            .map(|c| c.trim())
            .collect();
        if cells.len() < 4 || lane.is_empty() {
            continue;
        }
        out.push(Capability {
            id: cells[0].to_string(),
            lane: lane.clone(),
            name: cells[1].to_string(),
            revenue_form: cells[2].to_string(),
            proof: cells[3].to_string(),
        });
    }
    out
}

pub fn load(root: &Path) -> Ledger {
    let mut errors = 0usize;
    let opp_text = fs::read_to_string(root.join("OPPORTUNITIES.md")).unwrap_or_default();
    let cap_text = fs::read_to_string(root.join("CAPABILITIES.md")).unwrap_or_default();
    Ledger {
        opportunities: parse_opportunities(&opp_text, &mut errors),
        capabilities: parse_capabilities(&cap_text, &mut errors),
        parse_errors: errors,
    }
}

/// Lane visibility: an `sfw`-scoped caller never sees `nsfw` entries.
pub fn lane_visible(scope: &str, lane: &str) -> bool {
    scope == "operator" || lane == "sfw"
}

pub fn summary(ledger: &Ledger, scope: &str) -> serde_json::Value {
    let mut by_status: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_lane: BTreeMap<String, usize> = BTreeMap::new();
    for opp in &ledger.opportunities {
        if !lane_visible(scope, &opp.lane) {
            continue;
        }
        *by_status.entry(opp.status.clone()).or_default() += 1;
        *by_lane.entry(opp.lane.clone()).or_default() += 1;
    }
    serde_json::json!({
        "opportunities_by_status": by_status,
        "opportunities_by_lane": by_lane,
        "capabilities": ledger.capabilities.iter()
            .filter(|c| lane_visible(scope, &c.lane)).count(),
        "parse_errors": ledger.parse_errors,
    })
}

/// Advisory keyword match over the capability registry. Deterministic:
/// score by distinct query tokens matched, ties broken by capability id.
pub fn capability_match(ledger: &Ledger, scope: &str, query: &str) -> Vec<(String, usize)> {
    let tokens: Vec<String> = query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 3)
        .map(|t| t.to_string())
        .collect();
    let mut scored: Vec<(String, usize)> = ledger
        .capabilities
        .iter()
        .filter(|c| lane_visible(scope, &c.lane))
        .map(|c| {
            let hay = format!("{} {} {} {}", c.id, c.name, c.revenue_form, c.proof).to_lowercase();
            let score = tokens.iter().filter(|t| hay.contains(t.as_str())).count();
            (c.id.clone(), score)
        })
        .filter(|(_, s)| *s > 0)
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.truncate(5);
    scored
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEDGER_FIXTURE: &str = "\
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

    const BROKEN_FIXTURE: &str = "\
### OPP-SFW-009 — Broken Entry
- **Lane:** sfw · **Capability:** CAP-FATHOM
- **Pays:** unstated

### OPP-SFW-010 — Good Entry
- **Lane:** sfw · **Capability:** CAP-PHLOW
- **Pays:** unstated
- **Status:** queued
";

    const CAP_FIXTURE: &str = "\
## SFW lane — Qompass

| ID | Capability | Revenue form | Proof / state |
|----|-----------|--------------|----------------|
| CAP-FATHOM | fathom — Rust MCP reverse-engineering server | Product + services | Published |

## NSFW lane — NTRanch

| ID | Capability | Revenue form | Proof / state |
|----|-----------|--------------|----------------|
| CAP-STROBE-GAME | Strobe game | Game sales | In design |
";

    #[test]
    fn parses_valid_entries_with_fields() {
        let mut errors = 0;
        let opps = parse_opportunities(LEDGER_FIXTURE, &mut errors);
        assert_eq!(errors, 0);
        assert_eq!(opps.len(), 2);
        assert_eq!(opps[0].id, "OPP-SFW-001");
        assert_eq!(opps[0].lane, "sfw");
        assert_eq!(opps[0].status, "validated");
        assert_eq!(opps[0].deadline, "2026-11-03");
        assert_eq!(opps[1].lane, "nsfw");
    }

    #[test]
    fn malformed_entry_fails_closed_and_is_counted() {
        let mut errors = 0;
        let opps = parse_opportunities(BROKEN_FIXTURE, &mut errors);
        assert_eq!(errors, 1);
        assert_eq!(opps.len(), 1);
        assert_eq!(opps[0].id, "OPP-SFW-010");
    }

    #[test]
    fn capabilities_carry_lane_from_section() {
        let mut errors = 0;
        let caps = parse_capabilities(CAP_FIXTURE, &mut errors);
        assert_eq!(caps.len(), 2);
        assert_eq!(caps[0].lane, "sfw");
        assert_eq!(caps[1].lane, "nsfw");
    }

    #[test]
    fn sfw_scope_never_sees_nsfw() {
        assert!(lane_visible("operator", "nsfw"));
        assert!(lane_visible("sfw", "sfw"));
        assert!(!lane_visible("sfw", "nsfw"));
    }

    #[test]
    fn match_ranks_relevant_capability_first() {
        let mut errors = 0;
        let ledger = Ledger {
            opportunities: vec![],
            capabilities: parse_capabilities(CAP_FIXTURE, &mut errors),
            parse_errors: 0,
        };
        let ranked = capability_match(&ledger, "operator", "rust reverse engineering server");
        assert!(!ranked.is_empty());
        assert_eq!(ranked[0].0, "CAP-FATHOM");
    }

    #[test]
    fn match_is_deterministic() {
        let mut errors = 0;
        let ledger = Ledger {
            opportunities: vec![],
            capabilities: parse_capabilities(CAP_FIXTURE, &mut errors),
            parse_errors: 0,
        };
        let a = capability_match(&ledger, "operator", "game sales product");
        let b = capability_match(&ledger, "operator", "game sales product");
        assert_eq!(a, b);
    }
}
