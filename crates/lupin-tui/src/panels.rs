//! Pure ratatui panel constructors.
//!
//! Every panel is a pure function from data to widget: no terminal
//! access, no I/O, no facade calls. Tests render them through
//! ratatui's `TestBackend` and assert on the buffer text. Text that
//! comes from the server is truncated with the named bounds below
//! before it reaches a panel.

use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Paragraph};
use serde_json::Value;

use crate::facade::Lane;
use crate::screens::{Mode, StagedCall, Workflow};
use crate::triggers::TriggerRule;

/// Titles (opportunities, capabilities) truncate to this length.
pub const TITLE_CHARS_MAX: usize = 48;

/// Any single rendered line truncates to this length.
pub const LINE_CHARS_MAX: usize = 120;

/// Log lines shown in the Log panel at once.
pub const LOG_LINES_MAX: usize = 32;

/// Audit lines shown in the Fleet panel at once.
pub const AUDIT_LINES_MAX: usize = 8;

/// Truncate `text` to `max` characters, marking truncation with
/// `...` when the text had to be cut.
pub fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let keep = max.saturating_sub(3);
    let mut out: String = text.chars().take(keep).collect();
    out.push_str("...");
    out
}

fn titled(text: String, title: &str, color: Color) -> Paragraph<'static> {
    let style = Style::default().fg(color);
    Paragraph::new(text)
        .block(
            Block::bordered()
                .title(title.to_string())
                .border_style(style),
        )
        .wrap(ratatui::widgets::Wrap { trim: false })
}

/// The lane banner line shown on every data screen: which lane's
/// records are in view, and that the other lane is not reachable.
pub fn lane_banner(view: Lane) -> String {
    let other = match view {
        Lane::Sfw => "nsfw",
        Lane::Nsfw => "sfw",
    };
    format!("Lane view: {view} — {other} records are not reachable in this view")
}

/// Overview: server identity/status, tool count, trigger count,
/// and ledger counts — already lane-sanitised by the caller, so an
/// `sfw` view contains no `nsfw` numbers at all.
pub fn overview_panel(
    server: &str,
    version: &str,
    view: Lane,
    tool_count: usize,
    trigger_count: usize,
    summary: &Value,
) -> Paragraph<'static> {
    let mut text = format!("Server: {server} {version}\n");
    text.push_str(&lane_banner(view));
    text.push_str(&format!(
        "\nTools: {tool_count}  Triggers: {trigger_count}\n"
    ));
    if let Some(by_lane) = summary
        .get("opportunities_by_lane")
        .and_then(Value::as_object)
    {
        for (lane, count) in by_lane {
            text.push_str(&format!("Opportunities [{lane}]: {count}\n"));
        }
    }
    if let Some(by_status) = summary
        .get("opportunities_by_status")
        .and_then(Value::as_object)
    {
        for (status, count) in by_status {
            text.push_str(&format!("  status {status}: {count}\n"));
        }
    }
    if let Some(caps) = summary.get("capabilities") {
        text.push_str(&format!("Capabilities in view: {caps}\n"));
    }
    if let Some(errors) = summary.get("parse_errors") {
        text.push_str(&format!("Parse errors: {errors}"));
    }
    titled(text, "Lupin — Overview", Color::Green)
}

/// Workflows: the catalogue with the selection marked, the run
/// mode, and any staged call shown with its exact arguments.
pub fn workflows_panel(
    catalogue: &[Workflow],
    selected: usize,
    mode: Mode,
    staged: Option<&StagedCall>,
    view: Lane,
) -> Paragraph<'static> {
    let mut text = format!("Mode: {}   {}\n", mode.as_str(), lane_banner(view));
    text.push_str("Provider: n/a — lupin routes no model calls\n");
    for (index, workflow) in catalogue.iter().enumerate() {
        let marker = if index == selected { ">" } else { " " };
        let tag = if workflow.consequential {
            " [plan-only]"
        } else {
            ""
        };
        text.push_str(&format!(
            "{marker} {} ({}){tag}\n",
            workflow.name, workflow.tool
        ));
    }
    match staged {
        Some(call) => {
            text.push_str(&format!(
                "\nStaged: {}\nApprove (a) or abort (x).",
                call.describe()
            ));
        }
        None => text.push_str("\nNothing staged."),
    }
    titled(text, "Lupin — Workflows", Color::Cyan)
}

/// A record list (opportunities or capabilities): one line per
/// already-filtered record, titles truncated to the named bound.
pub fn records_panel(title: &str, records: &[Value], view: Lane) -> Paragraph<'static> {
    let mut text = format!("{}\n", lane_banner(view));
    if records.is_empty() {
        text.push_str("(none in view)");
    }
    for record in records {
        let id = record.get("id").and_then(Value::as_str).unwrap_or("?");
        let label = record
            .get("title")
            .or_else(|| record.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let lane = record.get("lane").and_then(Value::as_str).unwrap_or("?");
        let line = format!("{id} [{lane}] {}", truncate_chars(label, TITLE_CHARS_MAX));
        text.push_str(&truncate_chars(&line, LINE_CHARS_MAX));
        text.push('\n');
    }
    titled(text, title, Color::Cyan)
}

/// Log / Live: the bounded event buffer, newest last.
pub fn log_panel(lines: &[String]) -> Paragraph<'static> {
    let start = lines.len().saturating_sub(LOG_LINES_MAX);
    let mut text = String::new();
    for line in &lines[start..] {
        text.push_str(&truncate_chars(line, LINE_CHARS_MAX));
        text.push('\n');
    }
    if text.is_empty() {
        text.push_str("(no events yet)");
    }
    titled(text, "Lupin — Log / Live", Color::Yellow)
}

/// Fleet: the declared trigger rules (inbound and outbound) with
/// enable state and birth labels, plus the audit tail. Rules are
/// visible here and editable-off only; nothing fires that is not
/// listed here.
pub fn fleet_panel(rules: &[TriggerRule], audit: &[String], view: Lane) -> Paragraph<'static> {
    let mut text = format!("{}\n", lane_banner(view));
    text.push_str("Fleet: lupin (this member); phaedrus (draft source)\n");
    if rules.is_empty() {
        text.push_str("Rules: none declared\n");
    }
    for rule in rules {
        let direction = match rule.direction {
            crate::triggers::Direction::Inbound => "inbound",
            crate::triggers::Direction::Outbound => "outbound",
        };
        let source = rule.source_mcp.as_deref().unwrap_or("-");
        text.push_str(&format!(
            "[{}] {} {direction} {source} -> {}:{} on {} (mode {}, {}/{})\n",
            if rule.enabled { "on" } else { "off" },
            rule.id,
            rule.target_mcp,
            rule.target_tool,
            rule.on_event,
            match rule.mode {
                crate::triggers::TriggerMode::Auto => "auto",
                crate::triggers::TriggerMode::Plan => "plan",
            },
            rule.origin_lane,
            rule.content_label,
        ));
    }
    text.push_str("Audit:\n");
    let start = audit.len().saturating_sub(AUDIT_LINES_MAX);
    for line in &audit[start..] {
        text.push_str(&truncate_chars(line, LINE_CHARS_MAX));
        text.push('\n');
    }
    titled(text, "Lupin — Fleet / MCP", Color::Magenta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::triggers::TriggerRule;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use serde_json::json;

    fn render(panel: Paragraph<'static>) -> String {
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| frame.render_widget(panel, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut text = String::new();
        for row in buffer.content().chunks(buffer.area.width as usize) {
            for cell in row {
                text.push_str(cell.symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn validation_overview_renders_lane_and_counts() {
        let summary = json!({
            "opportunities_by_lane": {"sfw": 3},
            "opportunities_by_status": {"validated": 3},
            "capabilities": 5,
            "parse_errors": 0
        });
        let text = render(overview_panel("lupin", "1.0.0", Lane::Sfw, 6, 1, &summary));
        assert!(text.contains("Lane view: sfw"));
        assert!(text.contains("Opportunities [sfw]: 3"));
        assert!(text.contains("Tools: 6"));
    }

    #[test]
    fn validation_records_panel_shows_filtered_records() {
        let records = vec![json!({"id": "OPP-SFW-001", "title": "Grant", "lane": "sfw"})];
        let text = render(records_panel("Opportunities", &records, Lane::Sfw));
        assert!(text.contains("OPP-SFW-001"));
        assert!(text.contains("not reachable"));
    }

    #[test]
    fn validation_fleet_panel_lists_inbound_rule_with_labels() {
        let text = render(fleet_panel(
            &[TriggerRule::phaedrus_inbound()],
            &[],
            Lane::Sfw,
        ));
        assert!(text.contains("phaedrus-finding-to-lupin-draft"));
        assert!(text.contains("inbound"));
        assert!(text.contains("qompass/sfw"));
    }

    #[test]
    fn adversarial_oversized_title_is_truncated() {
        let long = "x".repeat(500);
        let records = vec![json!({"id": "A", "title": long, "lane": "sfw"})];
        let text = render(records_panel("Opportunities", &records, Lane::Sfw));
        assert!(!text.contains(&"x".repeat(200)));
        assert_eq!(
            truncate_chars(&"y".repeat(500), TITLE_CHARS_MAX)
                .chars()
                .count(),
            TITLE_CHARS_MAX
        );
    }
}
