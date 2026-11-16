//! Application state, the workflow execute path, and both run
//! modes (full-screen ratatui and non-TTY line mode share the same
//! [`App::execute_line`] path, per the phlow precedent).
//!
//! Lane discipline is enforced here, not just rendered: the app is
//! constructed for one scope lane (`sfw` in this build), its view
//! starts there, attempts to move the view to another lane are
//! refused, and every record list fetched through the facade is
//! filtered to the view lane before it can reach state or a panel.
//! Summaries are sanitised the same way, so an operator-scoped
//! server's cross-lane counts never render in an `sfw` view.
//!
//! Plan discipline is enforced here too: staging a call performs
//! **no** facade call. Only [`App::approve_staged`] executes a
//! staged call, and [`App::abort_staged`] discards it. Consequential
//! calls stage in every mode (see [`crate::screens`]).

use std::collections::VecDeque;
use std::io::{self, BufRead, Write};

use serde_json::{Value, json};

use crate::LOG_ENTRIES_MAX;
use crate::facade::{FacadeError, Lane, LupinFacade, filter_records};
use crate::panels;
use crate::screens::{Mode, Screen, StagedCall, Workflow, workflows};
use crate::triggers::{Event, TriggerEngine};

/// An input line longer than this many bytes is rejected rather
/// than buffered without bound.
pub const INPUT_LINE_BYTES_MAX: usize = 8192;

/// What the loop should do with one input line.
#[derive(Debug, PartialEq, Eq)]
pub enum LineAction {
    /// End the session (`quit` / `exit`).
    Quit,
    /// Ignore the line (empty after trimming).
    Skip,
    /// Execute it; carries the trimmed text.
    Execute(String),
}

/// Classify one line-mode input line. Pure and unit-testable.
pub fn classify_line(line: &str) -> LineAction {
    let text = line.trim();
    if text.is_empty() {
        LineAction::Skip
    } else if text == "quit" || text == "exit" {
        LineAction::Quit
    } else {
        LineAction::Execute(text.to_string())
    }
}

/// Remove every lane except `view` from a summary's by-lane counts
/// and recompute the in-view capability count from a filtered list
/// when present. Pure; the input summary is not mutated.
pub fn sanitise_summary(summary: &Value, view: Lane) -> Value {
    let mut out = summary.clone();
    if let Some(by_lane) = out
        .get_mut("opportunities_by_lane")
        .and_then(Value::as_object_mut)
    {
        by_lane.retain(|lane, _| lane == view.as_str());
    }
    out
}

/// Extract a named array from a tool result, failing typed when
/// the server answered a malformed shape (not an object, or the
/// field is not an array).
fn extract_array(result: &Value, field: &str) -> Result<Vec<Value>, FacadeError> {
    if !result.is_object() {
        return Err(FacadeError::Protocol(format!(
            "tool result is not a JSON object (got {})",
            result
        )));
    }
    match result.get(field) {
        Some(Value::Array(items)) => Ok(items.clone()),
        Some(_) => Err(FacadeError::Protocol(format!(
            "tool result field '{field}' is not an array"
        ))),
        None => Ok(Vec::new()),
    }
}

/// The application: facade, screen state machine, view lane, run
/// mode, staged call, bounded log, and the trigger engine.
pub struct App<F: LupinFacade> {
    facade: F,
    screen: Screen,
    scope_lane: Lane,
    view_lane: Lane,
    mode: Mode,
    selected: usize,
    staged: Option<StagedCall>,
    log: VecDeque<String>,
    triggers: TriggerEngine,
    overview_cache: Option<Value>,
    tool_count: usize,
    opportunities_cache: Vec<Value>,
    capabilities_cache: Vec<Value>,
}

impl<F: LupinFacade> App<F> {
    /// Build the app for one scope lane. The view starts at the
    /// scope lane and, for this build, can never leave it.
    pub fn new(facade: F, scope_lane: Lane, triggers: TriggerEngine) -> App<F> {
        App {
            facade,
            screen: Screen::Overview,
            scope_lane,
            view_lane: scope_lane,
            mode: Mode::Plan,
            selected: 0,
            staged: None,
            log: VecDeque::new(),
            triggers,
            overview_cache: None,
            tool_count: 0,
            opportunities_cache: Vec::new(),
            capabilities_cache: Vec::new(),
        }
    }

    /// The facade (tests inspect the fake's recorded calls here).
    pub fn facade(&mut self) -> &mut F {
        &mut self.facade
    }

    /// Current screen.
    pub fn screen(&self) -> Screen {
        self.screen
    }

    /// Move to a named screen.
    pub fn set_screen(&mut self, screen: Screen) {
        self.screen = screen;
    }

    /// Cycle to the next screen (Tab in the full-screen loop).
    pub fn next_screen(&mut self) {
        self.screen = self.screen.next();
    }

    /// The lane whose records are in view.
    pub fn view_lane(&self) -> Lane {
        self.view_lane
    }

    /// Current run mode.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Set the run mode.
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    /// The staged call awaiting approval, if any.
    pub fn staged(&self) -> Option<&StagedCall> {
        self.staged.as_ref()
    }

    /// The bounded event log, oldest first.
    pub fn log_lines(&self) -> Vec<String> {
        self.log.iter().cloned().collect()
    }

    /// The trigger engine (Fleet panel data).
    pub fn triggers(&self) -> &TriggerEngine {
        &self.triggers
    }

    /// Attempt to move the view to another lane. Refused unless the
    /// target is this build's scope lane: cross-lane records are
    /// not reachable from the wrong view, by construction here and
    /// by scope on the server.
    pub fn set_view_lane(&mut self, lane: Lane) -> Result<(), FacadeError> {
        if lane != self.scope_lane {
            return Err(FacadeError::Protocol(format!(
                "lane view '{lane}' is outside this TUI's scope lane '{}'",
                self.scope_lane
            )));
        }
        self.view_lane = lane;
        Ok(())
    }

    fn push_log(&mut self, line: String) {
        if self.log.len() >= LOG_ENTRIES_MAX {
            self.log.pop_front();
        }
        self.log.push_back(line);
    }

    /// Fetch and sanitise the Overview data. Cross-lane counts from
    /// an operator-scoped answer are stripped before caching.
    pub fn refresh_overview(&mut self) -> Result<Value, FacadeError> {
        let status = match self.facade.status() {
            Ok(status) => status,
            Err(error) => {
                self.push_log(format!("overview failed: {error}"));
                return Err(error);
            }
        };
        if let Ok(tools) = self.facade.tools() {
            self.tool_count = tools.len();
        }
        let summary = status.get("summary").cloned().unwrap_or(Value::Null);
        let clean = sanitise_summary(&summary, self.view_lane);
        let out = json!({
            "server": status.get("server").cloned().unwrap_or(json!("lupin")),
            "version": status.get("version").cloned().unwrap_or(json!("")),
            "summary": clean,
        });
        self.overview_cache = Some(out.clone());
        self.push_log("overview refreshed".to_string());
        Ok(out)
    }

    /// Fetch opportunities, filtered to the view lane. Returns the
    /// visible records; dropped cross-lane records are counted in
    /// the log but never returned or rendered.
    pub fn refresh_opportunities(&mut self) -> Result<Vec<Value>, FacadeError> {
        let result = self.facade.call("opportunity_list", json!({}))?;
        let records = extract_array(&result, "opportunities")?;
        let (visible, dropped) = filter_records(&records, self.view_lane);
        if dropped > 0 {
            self.push_log(format!(
                "lane filter dropped {dropped} record(s) outside view {}",
                self.view_lane
            ));
        }
        self.opportunities_cache = visible.clone();
        Ok(visible)
    }

    /// Fetch capabilities, filtered to the view lane (same contract
    /// as [`App::refresh_opportunities`]).
    pub fn refresh_capabilities(&mut self) -> Result<Vec<Value>, FacadeError> {
        let result = self.facade.call("capability_list", json!({}))?;
        let records = extract_array(&result, "capabilities")?;
        let (visible, dropped) = filter_records(&records, self.view_lane);
        if dropped > 0 {
            self.push_log(format!(
                "lane filter dropped {dropped} record(s) outside view {}",
                self.view_lane
            ));
        }
        self.capabilities_cache = visible.clone();
        Ok(visible)
    }

    /// Fetch one opportunity by id, refusing a cross-lane answer
    /// even if the server hands one over.
    pub fn get_opportunity(&mut self, id: &str) -> Result<Value, FacadeError> {
        let record = self.facade.call("opportunity_get", json!({"id": id}))?;
        let lane = record.get("lane").and_then(Value::as_str).unwrap_or("");
        if crate::facade::lane_visible(self.view_lane, lane) {
            Ok(record)
        } else {
            Err(FacadeError::Protocol(format!(
                "opportunity {id} is outside lane view {}",
                self.view_lane
            )))
        }
    }

    /// Stage or execute one workflow call.
    ///
    /// In plan mode — and for a consequential call in any mode —
    /// this stages the exact call and performs **no** facade call.
    /// Otherwise it executes directly. Returns a status JSON value
    /// describing which of those happened.
    pub fn run_workflow(&mut self, workflow: &Workflow, args: Value) -> Result<Value, FacadeError> {
        if workflow.tool == "opportunity_submit" {
            self.validate_draft_lane(&args)?;
        }
        if self.mode == Mode::Plan || workflow.consequential {
            self.staged = Some(StagedCall {
                tool: workflow.tool.to_string(),
                args,
                consequential: workflow.consequential,
            });
            self.push_log(format!("staged {} (awaiting approval)", workflow.tool));
            return Ok(json!({"status": "staged", "tool": workflow.tool,
                             "staged": self.staged.as_ref().map(StagedCall::describe)}));
        }
        self.execute_call(workflow.tool, args)
    }

    fn validate_draft_lane(&self, args: &Value) -> Result<(), FacadeError> {
        let lane = args.get("lane").and_then(Value::as_str).unwrap_or("");
        if crate::facade::lane_visible(self.view_lane, lane) {
            Ok(())
        } else {
            Err(FacadeError::Protocol(format!(
                "draft lane '{lane}' is outside lane view {}",
                self.view_lane
            )))
        }
    }

    /// Execute a call now (the shared execute path): facade call,
    /// `workflow.completed` event into the trigger engine, log line.
    pub fn execute_call(&mut self, tool: &str, args: Value) -> Result<Value, FacadeError> {
        match self.facade.call(tool, args) {
            Ok(value) => {
                self.push_log(format!("{tool} completed"));
                let mut event = Event::new("workflow.completed");
                event.fields.insert("error_count".to_string(), json!(0));
                self.triggers.evaluate(&event);
                Ok(json!({"status": "ok", "tool": tool, "result": value}))
            }
            Err(error) => {
                self.push_log(format!("{tool} failed: {error}"));
                Err(error)
            }
        }
    }

    /// Approve the staged call: the only path that executes one.
    pub fn approve_staged(&mut self) -> Result<Value, FacadeError> {
        let staged = self
            .staged
            .take()
            .ok_or_else(|| FacadeError::Protocol("nothing staged to approve".to_string()))?;
        self.push_log(format!("approved staged {}", staged.tool));
        self.execute_call(&staged.tool, staged.args)
    }

    /// Abort the staged call: discards it; no facade call happens.
    pub fn abort_staged(&mut self) -> Value {
        match self.staged.take() {
            Some(staged) => {
                self.push_log(format!("aborted staged {}", staged.tool));
                json!({"status": "aborted", "tool": staged.tool})
            }
            None => json!({"status": "noop", "note": "nothing staged"}),
        }
    }

    /// Render the current screen into a ratatui buffer area via the
    /// pure panel constructors, using cached data.
    pub fn draw(&self, frame: &mut ratatui::Frame<'_>) {
        let area = frame.area();
        match self.screen {
            Screen::Overview => {
                let cache = self.overview_cache.clone().unwrap_or(Value::Null);
                let summary = cache.get("summary").cloned().unwrap_or(Value::Null);
                let server = cache
                    .get("server")
                    .and_then(Value::as_str)
                    .unwrap_or("lupin");
                let version = cache.get("version").and_then(Value::as_str).unwrap_or("");
                let panel = panels::overview_panel(
                    server,
                    version,
                    self.view_lane,
                    self.tool_count,
                    self.triggers.rules().len(),
                    &summary,
                );
                frame.render_widget(panel, area);
            }
            Screen::Workflows => {
                let panel = panels::workflows_panel(
                    &workflows(),
                    self.selected,
                    self.mode,
                    self.staged.as_ref(),
                    self.view_lane,
                );
                frame.render_widget(panel, area);
            }
            Screen::Log => {
                frame.render_widget(panels::log_panel(&self.log_lines()), area);
            }
            Screen::Fleet => {
                let panel = panels::fleet_panel(
                    self.triggers.rules(),
                    self.triggers.audit(),
                    self.view_lane,
                );
                frame.render_widget(panel, area);
            }
        }
    }

    /// Execute one pre-trimmed line-mode command through the same
    /// execute path as the full-screen loop. Returns the result as
    /// a JSON value; errors are JSON too, never panics.
    pub fn execute_line(&mut self, text: &str) -> Value {
        let (verb, rest) = match text.find(' ') {
            Some(index) => (&text[..index], text[index + 1..].trim()),
            None => (text, ""),
        };
        match verb {
            "help" => json!({"status": "ok", "commands": [
                "overview", "tools", "opportunities", "capabilities", "summary",
                "get <id>", "match <text>", "draft <title>;<source>;<why>",
                "approve", "abort", "staged", "lane", "mode plan|auto",
                "screen overview|workflows|log|fleet", "fleet", "log", "quit"
            ]}),
            "overview" | "status" => line_result(self.refresh_overview()),
            "tools" => match self.facade.tools() {
                Ok(tools) => json!({"status": "ok", "tools": tools}),
                Err(e) => json!({"status": "error", "error": e.to_string()}),
            },
            "opportunities" => match self.refresh_opportunities() {
                Ok(records) => json!({"status": "ok", "lane": self.view_lane.as_str(),
                                      "opportunities": records}),
                Err(e) => json!({"status": "error", "error": e.to_string()}),
            },
            "capabilities" => match self.refresh_capabilities() {
                Ok(records) => json!({"status": "ok", "lane": self.view_lane.as_str(),
                                      "capabilities": records}),
                Err(e) => json!({"status": "error", "error": e.to_string()}),
            },
            "summary" => self.line_workflow("summary", json!({})),
            "get" => {
                if rest.is_empty() {
                    json!({"status": "error", "error": "Usage: get <id>"})
                } else {
                    match self.get_opportunity(rest) {
                        Ok(record) => json!({"status": "ok", "opportunity": record}),
                        Err(e) => json!({"status": "error", "error": e.to_string()}),
                    }
                }
            }
            "match" => self.line_workflow("match", json!({"text": rest})),
            "draft" => self.line_draft(rest),
            "approve" => line_result(self.approve_staged()),
            "abort" => self.abort_staged(),
            "staged" => json!({"status": "ok",
                                "staged": self.staged.as_ref().map(StagedCall::describe)}),
            "lane" => {
                if rest.is_empty() {
                    json!({"status": "ok", "lane": self.view_lane.as_str(),
                           "banner": panels::lane_banner(self.view_lane)})
                } else {
                    match Lane::parse(rest) {
                        Some(lane) => match self.set_view_lane(lane) {
                            Ok(()) => json!({"status": "ok", "lane": lane.as_str()}),
                            Err(e) => json!({"status": "error", "error": e.to_string()}),
                        },
                        None => {
                            json!({"status": "error", "error": format!("unknown lane '{rest}'")})
                        }
                    }
                }
            }
            "mode" => match rest {
                "plan" => {
                    self.mode = Mode::Plan;
                    json!({"status": "ok", "mode": "plan"})
                }
                "auto" => {
                    self.mode = Mode::Auto;
                    json!({"status": "ok", "mode": "auto"})
                }
                _ => json!({"status": "error", "error": "Usage: mode plan|auto"}),
            },
            "screen" => match Screen::parse(rest) {
                Some(screen) => {
                    self.screen = screen;
                    json!({"status": "ok", "screen": screen.title()})
                }
                None => json!({"status": "error", "error": format!("unknown screen '{rest}'")}),
            },
            "fleet" => json!({"status": "ok",
                               "rules": self.triggers.rules().len(),
                               "audit": self.triggers.audit()}),
            "log" => json!({"status": "ok", "log": self.log_lines()}),
            _ => json!({"status": "error", "error": format!("unknown command '{verb}'; try help")}),
        }
    }

    fn line_workflow(&mut self, verb: &str, args: Value) -> Value {
        let catalogue = workflows();
        let Some(workflow) = catalogue.iter().find(|w| w.verb == verb) else {
            return json!({"status": "error", "error": format!("no workflow '{verb}'")});
        };
        let workflow = workflow.clone();
        match self.run_workflow(&workflow, args) {
            Ok(value) => value,
            Err(e) => json!({"status": "error", "error": e.to_string()}),
        }
    }

    fn line_draft(&mut self, rest: &str) -> Value {
        let parts: Vec<&str> = rest.split(';').map(str::trim).collect();
        if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
            return json!({"status": "error",
                           "error": "Usage: draft <title>;<source>;<why_it_fits>"});
        }
        let args = json!({
            "title": parts[0], "source": parts[1],
            "lane": self.view_lane.as_str(), "why_it_fits": parts[2],
        });
        self.line_workflow("draft", args)
    }
}

/// Wrap a facade result as a line-mode JSON value.
fn line_result(result: Result<Value, FacadeError>) -> Value {
    match result {
        Ok(value) => json!({"status": "ok", "result": value}),
        Err(e) => json!({"status": "error", "error": e.to_string()}),
    }
}

/// Run the non-TTY line mode: banner, then one JSON result per
/// input line, sharing [`App::execute_line`] with the full-screen
/// loop. EOF or `quit` ends the session cleanly.
pub fn run_line_mode<F: LupinFacade, R: BufRead, W: Write>(
    app: &mut App<F>,
    mut input: R,
    mut output: W,
) -> io::Result<()> {
    writeln!(
        output,
        "Lupin TUI (line mode) — lane view: {}",
        app.view_lane()
    )?;
    writeln!(output, "Type help for commands. quit or Ctrl-D to exit.")?;
    loop {
        write!(output, "lupin > ")?;
        output.flush()?;
        let mut line = String::new();
        let read = input.read_line(&mut line)?;
        if read == 0 {
            break;
        }
        if line.len() > INPUT_LINE_BYTES_MAX {
            writeln!(
                output,
                "{}",
                json!({"status": "error",
                "error": format!("input line exceeds {INPUT_LINE_BYTES_MAX} bytes")})
            )?;
            continue;
        }
        match classify_line(&line) {
            LineAction::Quit => break,
            LineAction::Skip => continue,
            LineAction::Execute(text) => {
                writeln!(output, "{}", app.execute_line(&text))?;
            }
        }
    }
    Ok(())
}

/// Run the full-screen ratatui loop: Tab cycles screens, `r`
/// refreshes the current screen's data, `a` approves a staged
/// call, `x` aborts it, `m` toggles plan/auto, `q` quits.
pub fn run_fullscreen<F: LupinFacade>(app: &mut App<F>) -> io::Result<()> {
    use crossterm::event::read;
    use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
    use ratatui::Terminal;
    use ratatui::backend::CrosstermBackend;

    let _ = app.refresh_overview();
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let outcome = fullscreen_loop(app, &mut terminal, read);
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen
    )?;
    disable_raw_mode()?;
    outcome
}

fn fullscreen_loop<F: LupinFacade>(
    app: &mut App<F>,
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>,
    read_event: fn() -> io::Result<crossterm::event::Event>,
) -> io::Result<()> {
    use crossterm::event::{Event as CtEvent, KeyCode};
    loop {
        terminal.draw(|frame| app.draw(frame))?;
        if let CtEvent::Key(key) = read_event()? {
            match key.code {
                KeyCode::Char('q') => break,
                KeyCode::Tab => app.next_screen(),
                KeyCode::Char('r') => match app.screen() {
                    Screen::Overview => {
                        let _ = app.refresh_overview();
                    }
                    Screen::Workflows => {
                        let _ = app.refresh_opportunities();
                    }
                    _ => {}
                },
                KeyCode::Char('a') => {
                    let _ = app.approve_staged();
                }
                KeyCode::Char('x') => {
                    app.abort_staged();
                }
                KeyCode::Char('m') => {
                    app.set_mode(match app.mode() {
                        Mode::Plan => Mode::Auto,
                        Mode::Auto => Mode::Plan,
                    });
                }
                KeyCode::Char('1') => app.set_screen(Screen::Overview),
                KeyCode::Char('2') => app.set_screen(Screen::Workflows),
                KeyCode::Char('3') => app.set_screen(Screen::Log),
                KeyCode::Char('4') => app.set_screen(Screen::Fleet),
                _ => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facade::FakeFacade;
    use crate::triggers::TriggerEngine;
    use crate::triggers::default_rules;

    fn sfw_app() -> App<FakeFacade> {
        App::new(
            FakeFacade::seeded(),
            Lane::Sfw,
            TriggerEngine::new(default_rules()),
        )
    }

    #[test]
    fn validation_overview_sanitises_cross_lane_counts() {
        let mut app = sfw_app();
        let overview = app.refresh_overview().unwrap();
        let by_lane = &overview["summary"]["opportunities_by_lane"];
        assert_eq!(by_lane["sfw"], json!(1));
        assert!(by_lane.get("nsfw").is_none());
    }

    #[test]
    fn validation_opportunities_are_filtered_to_view() {
        let mut app = sfw_app();
        let visible = app.refresh_opportunities().unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0]["id"], "OPP-SFW-001");
        assert!(app.log_lines().iter().any(|l| l.contains("dropped 1")));
    }

    #[test]
    fn adversarial_nsfw_records_never_surface_anywhere() {
        let mut app = sfw_app();
        let visible = app.refresh_opportunities().unwrap();
        let caps = app.refresh_capabilities().unwrap();
        let blob = format!("{visible:?} {caps:?}");
        assert!(!blob.contains("NSFW"));
        assert!(!blob.contains("OPP-NSFW-001"));
        assert!(!blob.contains("CAP-STROBE-ASSETS"));
        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0]["id"], "CAP-FATHOM");
    }

    #[test]
    fn adversarial_view_lane_cannot_move_to_nsfw() {
        let mut app = sfw_app();
        assert!(app.set_view_lane(Lane::Nsfw).is_err());
        assert_eq!(app.view_lane(), Lane::Sfw);
        let out = app.execute_line("lane nsfw");
        assert_eq!(out["status"], "error");
        assert_eq!(app.view_lane(), Lane::Sfw);
    }

    #[test]
    fn adversarial_get_cross_lane_opportunity_is_refused() {
        let mut app = sfw_app();
        // The fake answers opportunity_get from its mixed fixture;
        // OPP-NSFW-001 exists there but must not be reachable.
        assert!(app.get_opportunity("OPP-NSFW-001").is_err());
        assert!(app.get_opportunity("OPP-SFW-001").is_ok());
    }

    #[test]
    fn validation_plan_mode_stages_without_calling() {
        let mut app = sfw_app();
        let out = app.execute_line("draft Grant X;phaedrus;fits the fathom capability");
        assert_eq!(out["status"], "staged");
        // No-call proof: the fake recorded zero calls of any kind.
        assert!(app.facade().calls.is_empty());
        let staged = app.staged().expect("a staged call");
        assert_eq!(staged.tool, "opportunity_submit");
        assert_eq!(staged.args["lane"], "sfw");
        assert_eq!(staged.args["title"], "Grant X");
    }

    #[test]
    fn validation_approve_executes_exactly_the_staged_call() {
        let mut app = sfw_app();
        app.execute_line("draft Grant X;phaedrus;fits");
        let out = app.approve_staged().unwrap();
        assert_eq!(out["status"], "ok");
        assert_eq!(app.facade().calls.len(), 1);
        assert_eq!(app.facade().calls[0].0, "opportunity_submit");
        assert!(app.staged().is_none());
    }

    #[test]
    fn adversarial_approve_then_abort_and_abort_without_stage() {
        let mut app = sfw_app();
        app.execute_line("draft Grant X;phaedrus;fits");
        let aborted = app.abort_staged();
        assert_eq!(aborted["status"], "aborted");
        assert!(app.facade().calls.is_empty());
        // Approving after abort must fail: nothing is staged.
        assert!(app.approve_staged().is_err());
        assert!(app.facade().calls.is_empty());
    }

    #[test]
    fn adversarial_consequential_stages_even_in_auto_mode() {
        let mut app = sfw_app();
        app.set_mode(Mode::Auto);
        let out = app.execute_line("draft Grant X;phaedrus;fits");
        assert_eq!(out["status"], "staged");
        assert!(app.facade().calls.is_empty());
    }

    #[test]
    fn validation_auto_mode_executes_reads_directly() {
        let mut app = sfw_app();
        app.set_mode(Mode::Auto);
        let out = app.execute_line("summary");
        assert_eq!(out["status"], "ok");
        assert_eq!(app.facade().calls.len(), 1);
    }

    #[test]
    fn adversarial_malformed_server_response_is_typed_error() {
        let mut app = sfw_app();
        app.facade().malformed_next = Some(json!("not an object"));
        let out = app.execute_line("opportunities");
        assert_eq!(out["status"], "error");
        assert!(out["error"].as_str().unwrap().contains("not a JSON object"));
        // A malformed array field is also a typed error, not a panic.
        app.facade().malformed_next = Some(json!({"opportunities": 42}));
        let out = app.execute_line("opportunities");
        assert_eq!(out["status"], "error");
    }

    #[test]
    fn adversarial_server_death_mid_call_is_typed_error() {
        let mut app = sfw_app();
        app.facade().die_on_tool = Some("pipeline_summary".to_string());
        let out = app.execute_line("overview");
        assert_eq!(out["status"], "error");
        assert!(out["error"].as_str().unwrap().contains("transport"));
        assert!(
            app.log_lines()
                .iter()
                .any(|l| l.contains("failed") || l.contains("overview"))
        );
    }

    #[test]
    fn validation_line_mode_answers_one_command() {
        let mut app = sfw_app();
        let input = b"opportunities\nquit\n".as_slice();
        let mut output: Vec<u8> = Vec::new();
        run_line_mode(&mut app, input, &mut output).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("OPP-SFW-001"));
        assert!(!text.contains("OPP-NSFW-001"));
    }

    #[test]
    fn validation_classify_line_variants() {
        assert_eq!(classify_line("  "), LineAction::Skip);
        assert_eq!(classify_line("quit"), LineAction::Quit);
        assert_eq!(
            classify_line(" tools "),
            LineAction::Execute("tools".to_string())
        );
    }

    #[test]
    fn validation_first_screen_renders_under_test_backend() {
        let mut app = sfw_app();
        app.refresh_overview().unwrap();
        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut text = String::new();
        for cell in buffer.content() {
            text.push_str(cell.symbol());
        }
        assert!(text.contains("Lane view: sfw"));
        assert!(text.contains("Overview"));
    }
}
