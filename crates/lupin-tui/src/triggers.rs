//! The cross-MCP trigger layer (DESIGN D4), as declared data plus
//! one typed, bounded rule engine.
//!
//! Rules live in `triggers.toml` in the member repo and are listed
//! in the Fleet panel; a rule that is not in the file and not in the
//! panel does not exist. Each rule carries `origin_lane` and
//! `content_label` from birth (BLOCH lane law, D-1030): for lupin
//! tonight those are `qompass` / `sfw` on every rule.
//!
//! Tonight lupin ships **no outbound rules**. It ships the inbound
//! side of DESIGN rule #3 as declared data: `phaedrus → lupin
//! opportunity_draft` is listed as an accepted inbound draft source
//! (a curated phaedrus finding may be drafted into lupin; lupin's
//! own lane rules apply on its side, and a draft is never a
//! committed ledger entry). Inbound declarations never fire from
//! this engine — they describe what this member accepts, not what
//! it sends.
//!
//! Engine semantics for outbound rules (present for the pattern,
//! exercised by tests): bounded `where` predicates over named event
//! fields (comparisons only, no eval), `plan` mode surfaces a
//! decision instead of firing, idempotency by event hash, a hard
//! depth cap, and an audit line for every match, firing, refusal,
//! and drop.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;

use serde_json::{Value, json};

use crate::AUDIT_ENTRIES_MAX;

/// Maximum number of rules one file may declare.
pub const RULES_MAX: usize = 64;

/// Hard ceiling for trigger depth, whatever a rule declares.
pub const DEPTH_CEILING: u32 = 2;

/// How a rule fires when it matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerMode {
    /// Execute the declared call directly.
    Auto,
    /// Surface for human approval; never fire unapproved.
    Plan,
}

/// Whether a rule describes calls this member makes (`Outbound`)
/// or drafts this member accepts from another member (`Inbound`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// This member fires the call when the event matches.
    Outbound,
    /// Another member may draft into this one; listed, never fired.
    Inbound,
}

/// One declared trigger rule (data, not code).
#[derive(Clone, Debug, PartialEq)]
pub struct TriggerRule {
    /// Stable rule identifier.
    pub id: String,
    /// Inbound declaration or outbound firing rule.
    pub direction: Direction,
    /// Source member for inbound rules (`phaedrus` tonight).
    pub source_mcp: Option<String>,
    /// Target member of the declared call.
    pub target_mcp: String,
    /// Target tool of the declared call.
    pub target_tool: String,
    /// Event name that can match this rule.
    pub on_event: String,
    /// Optional bounded predicate field name.
    pub where_field: Option<String>,
    /// Optional predicate operator: `>`, `<`, `==`, `!=`.
    pub where_op: Option<String>,
    /// Optional predicate comparison value (integer).
    pub where_value: Option<i64>,
    /// Birth lane label (BLOCH lane law): `qompass` for lupin.
    pub origin_lane: String,
    /// Birth content label: `sfw` for lupin.
    pub content_label: String,
    /// Fire mode.
    pub mode: TriggerMode,
    /// Whether the rule is currently enabled in the panel.
    pub enabled: bool,
    /// Maximum event depth at which this rule may fire.
    pub max_depth: u32,
}

impl TriggerRule {
    /// The one rule lupin ships tonight: the inbound side of DESIGN
    /// rule #3, `phaedrus → lupin opportunity_draft`, as data.
    pub fn phaedrus_inbound() -> TriggerRule {
        TriggerRule {
            id: "phaedrus-finding-to-lupin-draft".to_string(),
            direction: Direction::Inbound,
            source_mcp: Some("phaedrus".to_string()),
            target_mcp: "lupin".to_string(),
            target_tool: "opportunity_draft".to_string(),
            on_event: "finding.accepted".to_string(),
            where_field: None,
            where_op: None,
            where_value: None,
            origin_lane: "qompass".to_string(),
            content_label: "sfw".to_string(),
            mode: TriggerMode::Plan,
            enabled: true,
            max_depth: 1,
        }
    }
}

/// The rules lupin ships when no `triggers.toml` is loaded: exactly
/// the inbound declaration, and no outbound rules.
pub fn default_rules() -> Vec<TriggerRule> {
    vec![TriggerRule::phaedrus_inbound()]
}

/// A typed event on the member's own event stream.
#[derive(Clone, Debug)]
pub struct Event {
    /// Event name, e.g. `workflow.completed`.
    pub name: String,
    /// Named fields the `where` predicates read.
    pub fields: BTreeMap<String, Value>,
    /// Trigger depth of this event (0 = observed directly).
    pub depth: u32,
}

impl Event {
    /// A depth-0 event with no fields.
    pub fn new(name: &str) -> Event {
        Event {
            name: name.to_string(),
            fields: BTreeMap::new(),
            depth: 0,
        }
    }
}

/// What the engine decided about one rule for one event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Auto mode: the declared call may fire.
    Fire {
        /// Rule that matched.
        rule_id: String,
        /// Target tool to call.
        target_tool: String,
    },
    /// Plan mode: surface for approval; nothing has fired.
    Plan {
        /// Rule that matched.
        rule_id: String,
        /// Target tool awaiting approval.
        target_tool: String,
    },
    /// Matched but dropped, with the reason (depth, duplicate).
    Drop {
        /// Rule that matched.
        rule_id: String,
        /// Why it did not fire.
        reason: String,
    },
}

/// Why a rules file could not be loaded.
#[derive(Debug)]
pub struct TriggerError(pub String);

impl fmt::Display for TriggerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid triggers file: {}", self.0)
    }
}

impl std::error::Error for TriggerError {}

/// Parse a `triggers.toml` document. Unknown keys and missing
/// required fields are errors, never defaults — except the birth
/// labels, which default to this member's own `qompass` / `sfw`
/// when a rule omits them (and are validated when present).
pub fn parse_rules(text: &str) -> Result<Vec<TriggerRule>, TriggerError> {
    let root: toml::Value = text
        .parse()
        .map_err(|e| TriggerError(format!("not valid TOML: {e}")))?;
    let entries = root
        .get("rule")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| TriggerError("file must list [[rule]] entries".to_string()))?;
    if entries.len() > RULES_MAX {
        return Err(TriggerError(format!(
            "file lists {} rules; the bound is {RULES_MAX}",
            entries.len()
        )));
    }
    let mut rules = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        rules.push(parse_rule(index, entry)?);
    }
    Ok(rules)
}

fn parse_rule(index: usize, entry: &toml::Value) -> Result<TriggerRule, TriggerError> {
    let at = format!("rule[{index}]");
    let table = entry
        .as_table()
        .ok_or_else(|| TriggerError(format!("{at} must be a table")))?;
    let allowed = [
        "id",
        "direction",
        "source_mcp",
        "target_mcp",
        "target_tool",
        "on_event",
        "where_field",
        "where_op",
        "where_value",
        "origin_lane",
        "content_label",
        "mode",
        "enabled",
        "max_depth",
    ];
    for key in table.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(TriggerError(format!("{at} has unknown key '{key}'")));
        }
    }
    let req = |key: &str| -> Result<String, TriggerError> {
        table
            .get(key)
            .and_then(toml::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| TriggerError(format!("{at}.{key} must be a string")))
    };
    let opt = |key: &str| -> Option<String> {
        table
            .get(key)
            .and_then(toml::Value::as_str)
            .map(str::to_string)
    };
    let direction = match opt("direction").as_deref() {
        Some("inbound") => Direction::Inbound,
        Some("outbound") | None => Direction::Outbound,
        Some(other) => {
            return Err(TriggerError(format!(
                "{at}.direction must be 'inbound' or 'outbound', got '{other}'"
            )));
        }
    };
    let mode = match opt("mode").as_deref() {
        Some("auto") => TriggerMode::Auto,
        Some("plan") | None => TriggerMode::Plan,
        Some(other) => {
            return Err(TriggerError(format!(
                "{at}.mode must be 'auto' or 'plan', got '{other}'"
            )));
        }
    };
    let origin_lane = opt("origin_lane").unwrap_or_else(|| "qompass".to_string());
    let content_label = opt("content_label").unwrap_or_else(|| "sfw".to_string());
    if origin_lane != "qompass" || content_label != "sfw" {
        return Err(TriggerError(format!(
            "{at} carries origin_lane='{origin_lane}' content_label='{content_label}'; \
             lupin rules are born qompass/sfw"
        )));
    }
    let max_depth = table
        .get("max_depth")
        .and_then(toml::Value::as_integer)
        .unwrap_or(1);
    if max_depth < 0 || max_depth > i64::from(DEPTH_CEILING) {
        return Err(TriggerError(format!(
            "{at}.max_depth must be within 0..={DEPTH_CEILING}"
        )));
    }
    Ok(TriggerRule {
        id: req("id")?,
        direction,
        source_mcp: opt("source_mcp"),
        target_mcp: req("target_mcp")?,
        target_tool: req("target_tool")?,
        on_event: req("on_event")?,
        where_field: opt("where_field"),
        where_op: opt("where_op"),
        where_value: table.get("where_value").and_then(toml::Value::as_integer),
        origin_lane,
        content_label,
        mode,
        enabled: table
            .get("enabled")
            .and_then(toml::Value::as_bool)
            .unwrap_or(true),
        max_depth: max_depth as u32,
    })
}

/// Deterministic FNV-1a hash for idempotency keys (no external
/// hasher; the key never leaves the process, it only needs to be
/// stable within a run and distinct per payload).
fn event_key(rule_id: &str, event: &Event) -> String {
    let payload = json!({
        "rule": rule_id,
        "event": event.name,
        "fields": event.fields,
        "depth": event.depth,
    })
    .to_string();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in payload.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn predicate_matches(rule: &TriggerRule, event: &Event) -> bool {
    let (Some(field), Some(op), Some(want)) = (&rule.where_field, &rule.where_op, rule.where_value)
    else {
        return true;
    };
    let Some(got) = event.fields.get(field).and_then(Value::as_i64) else {
        return false;
    };
    match op.as_str() {
        ">" => got > want,
        "<" => got < want,
        "==" => got == want,
        "!=" => got != want,
        _ => false,
    }
}

/// The rule engine: evaluates events against the declared rules,
/// records every outcome in a bounded audit log, and never fires
/// an inbound declaration, a disabled rule, a duplicate, or an
/// event past the depth cap.
#[derive(Debug)]
pub struct TriggerEngine {
    rules: Vec<TriggerRule>,
    audit: Vec<String>,
    fired_keys: BTreeSet<String>,
}

impl TriggerEngine {
    /// Build an engine over declared rules.
    pub fn new(rules: Vec<TriggerRule>) -> TriggerEngine {
        TriggerEngine {
            rules,
            audit: Vec::new(),
            fired_keys: BTreeSet::new(),
        }
    }

    /// The declared rules, as listed in the Fleet panel.
    pub fn rules(&self) -> &[TriggerRule] {
        &self.rules
    }

    /// The bounded audit log (JSON lines, oldest first).
    pub fn audit(&self) -> &[String] {
        &self.audit
    }

    /// Enable or disable one rule by id, effective immediately
    /// (the Fleet panel's one edit). Returns false for unknown ids.
    pub fn set_enabled(&mut self, rule_id: &str, enabled: bool) -> bool {
        match self.rules.iter_mut().find(|r| r.id == rule_id) {
            Some(rule) => {
                rule.enabled = enabled;
                true
            }
            None => false,
        }
    }

    fn push_audit(&mut self, rule: &TriggerRule, event: &Event, action: &str) {
        let line = json!({
            "rule": rule.id,
            "event": event.name,
            "action": action,
            "origin_lane": rule.origin_lane,
            "content_label": rule.content_label,
        })
        .to_string();
        if self.audit.len() >= AUDIT_ENTRIES_MAX {
            self.audit.remove(0);
        }
        self.audit.push(line);
    }

    /// Evaluate one event against every rule. Inbound declarations
    /// are audited as declarations and never produce a firing.
    pub fn evaluate(&mut self, event: &Event) -> Vec<Decision> {
        let mut decisions = Vec::new();
        let rules = self.rules.clone();
        for rule in &rules {
            if rule.on_event != event.name || !predicate_matches(rule, event) {
                continue;
            }
            if rule.direction == Direction::Inbound {
                self.push_audit(rule, event, "inbound-declared");
                continue;
            }
            if !rule.enabled {
                self.push_audit(rule, event, "refused-disabled");
                continue;
            }
            if event.depth > rule.max_depth || event.depth > DEPTH_CEILING {
                self.push_audit(rule, event, "dropped-depth");
                decisions.push(Decision::Drop {
                    rule_id: rule.id.clone(),
                    reason: format!("depth {} exceeds max_depth {}", event.depth, rule.max_depth),
                });
                continue;
            }
            let key = event_key(&rule.id, event);
            if !self.fired_keys.insert(key) {
                self.push_audit(rule, event, "dropped-duplicate");
                decisions.push(Decision::Drop {
                    rule_id: rule.id.clone(),
                    reason: "duplicate event (idempotency)".to_string(),
                });
                continue;
            }
            match rule.mode {
                TriggerMode::Auto => {
                    self.push_audit(rule, event, "fire");
                    decisions.push(Decision::Fire {
                        rule_id: rule.id.clone(),
                        target_tool: rule.target_tool.clone(),
                    });
                }
                TriggerMode::Plan => {
                    self.push_audit(rule, event, "plan");
                    decisions.push(Decision::Plan {
                        rule_id: rule.id.clone(),
                        target_tool: rule.target_tool.clone(),
                    });
                }
            }
        }
        decisions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outbound_rule(mode: TriggerMode) -> TriggerRule {
        TriggerRule {
            id: "test-outbound".to_string(),
            direction: Direction::Outbound,
            source_mcp: None,
            target_mcp: "bloch".to_string(),
            target_tool: "lesson_candidate".to_string(),
            on_event: "workflow.completed".to_string(),
            where_field: Some("error_count".to_string()),
            where_op: Some(">".to_string()),
            where_value: Some(0),
            origin_lane: "qompass".to_string(),
            content_label: "sfw".to_string(),
            mode,
            enabled: true,
            max_depth: 1,
        }
    }

    fn event_with_errors(count: i64) -> Event {
        let mut event = Event::new("workflow.completed");
        event.fields.insert("error_count".to_string(), json!(count));
        event
    }

    #[test]
    fn validation_shipped_rules_are_inbound_only() {
        let rules = default_rules();
        assert_eq!(rules.len(), 1);
        let rule = &rules[0];
        assert_eq!(rule.direction, Direction::Inbound);
        assert_eq!(rule.source_mcp.as_deref(), Some("phaedrus"));
        assert_eq!(rule.target_mcp, "lupin");
        assert_eq!(rule.target_tool, "opportunity_draft");
        assert_eq!(rule.origin_lane, "qompass");
        assert_eq!(rule.content_label, "sfw");
    }

    #[test]
    fn validation_inbound_rule_never_fires() {
        let mut engine = TriggerEngine::new(default_rules());
        let decisions = engine.evaluate(&Event::new("finding.accepted"));
        assert!(decisions.is_empty());
        assert_eq!(engine.audit().len(), 1);
        assert!(engine.audit()[0].contains("inbound-declared"));
        assert!(engine.audit()[0].contains("\"origin_lane\":\"qompass\""));
    }

    #[test]
    fn validation_auto_rule_fires_on_predicate_match() {
        let mut engine = TriggerEngine::new(vec![outbound_rule(TriggerMode::Auto)]);
        let decisions = engine.evaluate(&event_with_errors(3));
        assert_eq!(
            decisions,
            vec![Decision::Fire {
                rule_id: "test-outbound".to_string(),
                target_tool: "lesson_candidate".to_string(),
            }]
        );
        // Predicate fails: no decision.
        assert!(engine.evaluate(&event_with_errors(0)).is_empty());
    }

    #[test]
    fn validation_plan_rule_surfaces_instead_of_firing() {
        let mut engine = TriggerEngine::new(vec![outbound_rule(TriggerMode::Plan)]);
        let decisions = engine.evaluate(&event_with_errors(1));
        assert!(matches!(decisions[0], Decision::Plan { .. }));
    }

    #[test]
    fn adversarial_duplicate_event_is_dropped() {
        let mut engine = TriggerEngine::new(vec![outbound_rule(TriggerMode::Auto)]);
        assert_eq!(engine.evaluate(&event_with_errors(2)).len(), 1);
        let second = engine.evaluate(&event_with_errors(2));
        assert!(matches!(second[0], Decision::Drop { .. }));
    }

    #[test]
    fn adversarial_depth_beyond_cap_is_dropped() {
        let mut engine = TriggerEngine::new(vec![outbound_rule(TriggerMode::Auto)]);
        let mut deep = event_with_errors(5);
        deep.depth = 2;
        let decisions = engine.evaluate(&deep);
        assert!(matches!(decisions[0], Decision::Drop { .. }));
    }

    #[test]
    fn adversarial_disabled_rule_does_not_fire() {
        let mut rule = outbound_rule(TriggerMode::Auto);
        rule.enabled = false;
        let mut engine = TriggerEngine::new(vec![rule]);
        assert!(engine.evaluate(&event_with_errors(9)).is_empty());
        assert!(engine.audit()[0].contains("refused-disabled"));
    }

    #[test]
    fn validation_parse_rules_file_round_trip() {
        let text = "[[rule]]\n\
            id = \"phaedrus-finding-to-lupin-draft\"\n\
            direction = \"inbound\"\n\
            source_mcp = \"phaedrus\"\n\
            target_mcp = \"lupin\"\n\
            target_tool = \"opportunity_draft\"\n\
            on_event = \"finding.accepted\"\n\
            origin_lane = \"qompass\"\n\
            content_label = \"sfw\"\n\
            mode = \"plan\"\n";
        let rules = parse_rules(text).unwrap();
        assert_eq!(rules, default_rules());
    }

    #[test]
    fn adversarial_parse_rejects_foreign_lane_labels() {
        let text = "[[rule]]\n\
            id = \"x\"\n\
            target_mcp = \"lupin\"\n\
            target_tool = \"opportunity_draft\"\n\
            on_event = \"finding.accepted\"\n\
            origin_lane = \"ntranch\"\n\
            content_label = \"nsfw\"\n";
        assert!(parse_rules(text).is_err());
    }

    #[test]
    fn adversarial_parse_rejects_unknown_keys_and_bad_depth() {
        let unknown = "[[rule]]\nid = \"x\"\ntarget_mcp = \"l\"\ntarget_tool = \"t\"\n\
                       on_event = \"e\"\nsurprise = 1\n";
        assert!(parse_rules(unknown).is_err());
        let depth = "[[rule]]\nid = \"x\"\ntarget_mcp = \"l\"\ntarget_tool = \"t\"\n\
                     on_event = \"e\"\nmax_depth = 99\n";
        assert!(parse_rules(depth).is_err());
    }
}
