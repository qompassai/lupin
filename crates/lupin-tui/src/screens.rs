//! Screen definitions and the workflow catalogue: the member's
//! comparable-capability core (DESIGN D2/D3).
//!
//! Screens are `Overview`, `Workflows`, `Log`, and `Fleet`. There
//! is deliberately no Media screen — lupin's use case involves no
//! images, video, or diagrams, and D2 says absent, not stubbed.
//!
//! Workflows are the member's primary operations. Read workflows
//! execute directly in `auto` mode; the one consequential workflow
//! (filing an opportunity draft, which writes a candidate into the
//! server's scan inbox) is staged for approval in `plan` mode —
//! and stays staged even in `auto` mode, because a consequential
//! call may only fire unapproved when the *target's* own gate
//! authorises it (DESIGN D4), and lupin's server has no such gate
//! for submits. Provider routing is honestly absent: lupin routes
//! no model calls, so no provider control exists to fake.

use serde_json::Value;

/// One of the four screens. The state machine is this enum: an
/// invalid screen is unrepresentable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    /// Server status and lane-scoped ledger counts.
    Overview,
    /// Browse/create workflows with plan/auto staging.
    Workflows,
    /// Bounded live event log.
    Log,
    /// Fleet members, declared trigger rules, audit tail.
    Fleet,
}

impl Screen {
    /// All screens in tab order.
    pub const ALL: [Screen; 4] = [
        Screen::Overview,
        Screen::Workflows,
        Screen::Log,
        Screen::Fleet,
    ];

    /// Display title.
    pub fn title(self) -> &'static str {
        match self {
            Screen::Overview => "Overview",
            Screen::Workflows => "Workflows",
            Screen::Log => "Log / Live",
            Screen::Fleet => "Fleet / MCP",
        }
    }

    /// The next screen in tab order (wraps).
    pub fn next(self) -> Screen {
        match self {
            Screen::Overview => Screen::Workflows,
            Screen::Workflows => Screen::Log,
            Screen::Log => Screen::Fleet,
            Screen::Fleet => Screen::Overview,
        }
    }

    /// Parse a screen name from line mode (`overview`, `workflows`,
    /// `log`, `fleet`); anything else is not a screen.
    pub fn parse(text: &str) -> Option<Screen> {
        match text {
            "overview" => Some(Screen::Overview),
            "workflows" => Some(Screen::Workflows),
            "log" => Some(Screen::Log),
            "fleet" => Some(Screen::Fleet),
            _ => None,
        }
    }
}

/// Run mode for workflows: stage for approval, or execute declared
/// (non-consequential) operations directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Stage the exact tool call + args; wait for approval.
    Plan,
    /// Execute non-consequential operations directly.
    Auto,
}

impl Mode {
    /// Wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Plan => "plan",
            Mode::Auto => "auto",
        }
    }
}

/// One workflow in the catalogue.
#[derive(Clone, Debug, PartialEq)]
pub struct Workflow {
    /// Operator-facing name.
    pub name: &'static str,
    /// Line-mode verb that selects it.
    pub verb: &'static str,
    /// The MCP tool it calls.
    pub tool: &'static str,
    /// Whether the call writes anything on the server side.
    pub consequential: bool,
}

/// The workflow catalogue: lupin's primary operations.
pub fn workflows() -> Vec<Workflow> {
    vec![
        Workflow {
            name: "List opportunities",
            verb: "opportunities",
            tool: "opportunity_list",
            consequential: false,
        },
        Workflow {
            name: "Get one opportunity",
            verb: "get",
            tool: "opportunity_get",
            consequential: false,
        },
        Workflow {
            name: "File opportunity draft (candidate)",
            verb: "draft",
            tool: "opportunity_submit",
            consequential: true,
        },
        Workflow {
            name: "List capabilities",
            verb: "capabilities",
            tool: "capability_list",
            consequential: false,
        },
        Workflow {
            name: "Match capabilities (advisory)",
            verb: "match",
            tool: "capability_match",
            consequential: false,
        },
        Workflow {
            name: "Pipeline summary",
            verb: "summary",
            tool: "pipeline_summary",
            consequential: false,
        },
    ]
}

/// A staged tool call: the exact tool and arguments, shown to the
/// operator before anything fires. Created by plan mode (and by
/// any consequential call in any mode); executed only by an
/// explicit approval, discarded by an abort.
#[derive(Clone, Debug, PartialEq)]
pub struct StagedCall {
    /// Tool name to call on approval.
    pub tool: String,
    /// Exact arguments to send on approval.
    pub args: Value,
    /// Whether the call is consequential (writes on the server).
    pub consequential: bool,
}

impl StagedCall {
    /// One-line rendering of the staged call for panels and logs:
    /// tool name plus the exact argument JSON.
    pub fn describe(&self) -> String {
        format!("{} {}", self.tool, self.args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_screen_cycle_and_parse() {
        assert_eq!(Screen::Overview.next(), Screen::Workflows);
        assert_eq!(Screen::Fleet.next(), Screen::Overview);
        assert_eq!(Screen::parse("fleet"), Some(Screen::Fleet));
        assert_eq!(Screen::ALL.len(), 4);
    }

    #[test]
    fn adversarial_no_media_screen_exists() {
        assert_eq!(Screen::parse("media"), None);
        assert!(Screen::ALL.iter().all(|s| s.title() != "Media"));
    }

    #[test]
    fn validation_only_submit_is_consequential() {
        let catalogue = workflows();
        let consequential: Vec<&str> = catalogue
            .iter()
            .filter(|w| w.consequential)
            .map(|w| w.tool)
            .collect();
        assert_eq!(consequential, vec!["opportunity_submit"]);
    }
}
