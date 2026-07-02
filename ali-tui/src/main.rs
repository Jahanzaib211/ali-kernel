mod app;
mod config;
mod gateway;

use anyhow::Result;
use app::{human_chars, App, AppMode, PendingApproval, RiskLevel, SavingsStats};
use config::{Theme, TuiConfig};
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind,
    },
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::StreamExt;
use gateway::GatewayClient;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::Stylize,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame, Terminal,
};
use serde_json::Value;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

const SCROLL_PAGE: usize = 10;

fn tools_as_values() -> Vec<Value> {
    let tools = [
        r#"{"type":"function","function":{"name":"read_file","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}}}"#,
        r#"{"type":"function","function":{"name":"bash_exec","parameters":{"type":"object","properties":{"command":{"type":"string"}},"required":["command"]}}}"#,
        r#"{"type":"function","function":{"name":"crate_search","parameters":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}}}"#,
        r#"{"type":"function","function":{"name":"docs_lookup","parameters":{"type":"object","properties":{"crate":{"type":"string"}},"required":["crate"]}}}"#,
    ];
    tools
        .iter()
        .filter_map(|t| serde_json::from_str(t).ok())
        .collect()
}

// --- transcript ---

fn role_prefix<'a>(role: &str, t: &Theme) -> Span<'a> {
    match role {
        "user" => Span::raw("you ▸ ").bold().fg(t.user_msg),
        "assistant" => Span::raw("ali ▸ ").bold().fg(t.assistant_msg),
        "tool" => Span::raw("tool ▸ ").bold().fg(t.tool_msg),
        "system" => Span::raw("sys ▸ ").bold().fg(t.muted),
        _ => Span::raw("? ▸ ").fg(t.muted),
    }
}

fn transcript_lines<'a>(app: &'a App, t: &Theme) -> Vec<Line<'a>> {
    let mut lines: Vec<Line> = Vec::new();
    for msg in &app.messages {
        let content_fg = if msg.role == "system" { t.muted } else { t.fg };
        let mut first = true;
        for l in msg.content.lines() {
            if first {
                lines.push(Line::from(vec![
                    role_prefix(&msg.role, t),
                    Span::raw(l).fg(content_fg),
                ]));
                first = false;
            } else {
                lines.push(Line::from(vec![
                    Span::raw("      "),
                    Span::raw(l).fg(content_fg),
                ]));
            }
        }
        if first {
            lines.push(Line::from(vec![role_prefix(&msg.role, t)]));
        }
        lines.push(Line::from(""));
    }
    if !app.streaming_content.is_empty() {
        let mut first = true;
        for l in app.streaming_content.lines() {
            if first {
                lines.push(Line::from(vec![
                    role_prefix("assistant", t),
                    Span::raw(l).fg(t.fg),
                ]));
                first = false;
            } else {
                lines.push(Line::from(vec![Span::raw("      "), Span::raw(l).fg(t.fg)]));
            }
        }
        if app.is_typing {
            lines.push(Line::from(vec![Span::raw("      ▍").fg(t.warning)]));
        }
    }
    lines
}

/// Estimate how many terminal rows `lines` occupy after wrapping to `width`.
fn wrapped_height(lines: &[Line], width: u16) -> usize {
    let w = width.max(1) as usize;
    lines
        .iter()
        .map(|l| {
            let chars: usize = l.spans.iter().map(|s| s.content.chars().count()).sum();
            chars.div_ceil(w).max(1)
        })
        .sum()
}

fn render_transcript(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let lines = transcript_lines(app, t);
    let inner_w = area.width.saturating_sub(2);
    let inner_h = area.height.saturating_sub(2) as usize;

    let total = wrapped_height(&lines, inner_w);
    let max_scroll = total.saturating_sub(inner_h);
    let from_bottom = app.scroll_offset.min(max_scroll);
    let top_offset = max_scroll - from_bottom;

    let title = if from_bottom > 0 {
        format!(" Transcript ─ ↑{} (End to follow) ", from_bottom)
    } else {
        " Transcript ".to_string()
    };

    let para = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((top_offset as u16, 0))
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(ratatui::style::Style::default().fg(t.border)),
        );
    f.render_widget(para, area);
}

// --- right column: kernel snapshot + live feed ---

fn render_kernel_panel(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let dash = "—";
    let p = &app.pipeline;
    let kv = |k: &'static str, v: String, c: ratatui::style::Color| {
        Line::from(vec![Span::raw(k).dim(), Span::raw(v).fg(c)])
    };
    let critique = match p.l8_critique {
        Some(true) => ("yes".to_string(), t.success),
        Some(false) => ("no".to_string(), t.muted),
        None => (dash.to_string(), t.muted),
    };
    let lines = vec![
        kv(
            " route    ",
            p.l3_route.clone().unwrap_or_else(|| dash.into()),
            t.accent,
        ),
        kv(
            " memories ",
            p.l4_memories
                .map(|n| n.to_string())
                .unwrap_or_else(|| dash.into()),
            t.accent,
        ),
        kv(
            " messages ",
            p.l5_messages
                .map(|n| n.to_string())
                .unwrap_or_else(|| dash.into()),
            t.accent,
        ),
        kv(
            " compress ",
            p.l5_5_compression.clone().unwrap_or_else(|| dash.into()),
            t.accent2,
        ),
        kv(
            " tool     ",
            p.l7_tool.clone().unwrap_or_else(|| dash.into()),
            t.warning,
        ),
        kv(" critique ", critique.0, critique.1),
        kv(
            " tokens   ",
            p.l9_tokens.clone().unwrap_or_else(|| dash.into()),
            t.accent2,
        ),
    ];
    let para = Paragraph::new(lines).block(
        Block::default()
            .title(" Kernel ")
            .borders(Borders::ALL)
            .border_style(ratatui::style::Style::default().fg(t.border)),
    );
    f.render_widget(para, area);
}

fn render_savings_panel(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let dash = "—";
    let lines = match &app.savings {
        Some(s) => vec![
            Line::from(vec![
                Span::raw(" today    ").dim(),
                Span::raw(format!("${:.4} spent", s.today_spend_usd)).fg(t.fg),
                Span::raw(" · ").dim(),
                Span::raw(format!("${:.4} saved", s.today_saved_usd)).fg(t.success),
                Span::raw(format!(" ({} req)", s.today_requests)).dim(),
            ]),
            Line::from(vec![
                Span::raw(" all time ").dim(),
                Span::raw(format!("${:.4} spent", s.all_spend_usd)).fg(t.fg),
                Span::raw(" · ").dim(),
                Span::raw(format!("${:.4} saved", s.all_saved_usd)).fg(t.success),
                Span::raw(format!(" ({} req)", s.all_requests)).dim(),
            ]),
            Line::from(vec![
                Span::raw(" squeeze  ").dim(),
                Span::raw(format!("{:.1}% smaller requests", s.all_compression_pct)).fg(t.accent2),
                Span::raw(" · logs/usage.jsonl").dim(),
            ]),
        ],
        None => vec![Line::from(vec![
            Span::raw(" ").dim(),
            Span::raw(dash).dim(),
            Span::raw(" waiting for gateway /stats").dim(),
        ])],
    };
    let para = Paragraph::new(lines).block(
        Block::default()
            .title(" $ Savings (billed usage) ")
            .borders(Borders::ALL)
            .border_style(ratatui::style::Style::default().fg(t.border)),
    );
    f.render_widget(para, area);
}

fn render_live_feed(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let inner_h = area.height.saturating_sub(2) as usize;
    let inner_w = area.width.saturating_sub(2) as usize;

    let lines: Vec<Line> = app
        .live_events
        .iter()
        .rev()
        .take(inner_h)
        .rev()
        .map(|ev| {
            let budget = inner_w.saturating_sub(9 + ev.tag.len() + 2);
            let text: String = if ev.text.chars().count() > budget {
                let cut: String = ev.text.chars().take(budget.saturating_sub(1)).collect();
                format!("{}…", cut)
            } else {
                ev.text.clone()
            };
            Line::from(vec![
                Span::raw(format!("{} ", ev.ts)).dim(),
                Span::raw(format!("{} ", ev.tag)).bold().fg(ev.tag_color()),
                Span::raw(text).fg(t.fg),
            ])
        })
        .collect();

    let title = format!(
        " Live ─ {} squeezes · saved {} chars ",
        app.compress_events,
        human_chars(app.saved_chars)
    );
    let para = Paragraph::new(lines).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(ratatui::style::Style::default().fg(t.border)),
    );
    f.render_widget(para, area);
}

// --- command mode panels (unchanged behavior, restyled) ---

fn render_config_panel(f: &mut Frame, area: Rect, cfg: &TuiConfig, t: &Theme) {
    let lines = vec![
        Line::from(vec![
            Span::raw(" gateway_port:  "),
            Span::raw(format!("{}", cfg.gateway_port)).fg(t.accent),
        ]),
        Line::from(vec![
            Span::raw(" llama_port:    "),
            Span::raw(format!("{}", cfg.llama_port)).fg(t.accent),
        ]),
        Line::from(vec![
            Span::raw(" human_gate:    "),
            Span::raw(format!("{}", cfg.human_gate_enabled)).fg(t.warning),
        ]),
        Line::from(vec![
            Span::raw(" backend_url:   "),
            Span::raw(&cfg.backend_url).fg(t.accent),
        ]),
        Line::from(vec![
            Span::raw(" backend_model: "),
            Span::raw(&cfg.backend_model).fg(t.accent),
        ]),
        Line::from(vec![
            Span::raw(" claude_enabled:"),
            Span::raw(format!(" {}", cfg.claude_enabled)).fg(t.warning),
        ]),
        Line::from(vec![
            Span::raw(" tui_theme:     "),
            Span::raw(&cfg.tui_theme).fg(t.success),
        ]),
        Line::from(vec![
            Span::raw(" version:       ").dim(),
            Span::raw(&cfg.identity_version).dim(),
        ]),
    ];
    let para = Paragraph::new(lines).block(
        Block::default()
            .title(" Config ")
            .borders(Borders::ALL)
            .border_style(ratatui::style::Style::default().fg(t.border)),
    );
    f.render_widget(para, area);
}

fn render_command_help(f: &mut Frame, area: Rect, t: &Theme) {
    let lines = vec![
        Line::from(vec![Span::raw(" SLASH ").bold().white().on_black()]),
        Line::from(vec![
            Span::raw("/help /clear /retry /tools /status /exit").cyan()
        ]),
        Line::from(""),
        Line::from(vec![Span::raw(" COLON ").bold().white().on_black()]),
        Line::from(vec![
            Span::raw(":set <k> <v>   ").fg(t.success),
            Span::raw("session config").dim(),
        ]),
        Line::from(vec![
            Span::raw(":setg <k> <v>  ").fg(t.success),
            Span::raw("set + persist").dim(),
        ]),
        Line::from(vec![
            Span::raw(":get <k> / :show").fg(t.accent),
            Span::raw("  inspect config").dim(),
        ]),
        Line::from(vec![
            Span::raw(":save / :load  ").fg(t.warning),
            Span::raw("persist / reload").dim(),
        ]),
        Line::from(vec![Span::raw(":identity :status :tools").fg(t.accent2)]),
        Line::from(vec![
            Span::raw(":quit          ").fg(t.error),
            Span::raw("exit").dim(),
        ]),
    ];
    let para = Paragraph::new(lines).block(
        Block::default()
            .title(" Commands ")
            .borders(Borders::ALL)
            .border_style(ratatui::style::Style::default().fg(t.border)),
    );
    f.render_widget(para, area);
}

// --- approval / input / chrome ---

fn render_approval_card(f: &mut Frame, area: Rect, approvals: &[PendingApproval], t: &Theme) {
    if approvals.is_empty() {
        return;
    }
    let approval = &approvals[0];
    let risk_color = approval.risk.color();
    let args_str = serde_json::to_string(&approval.args).unwrap_or_default();
    let args_preview: String = if args_str.chars().count() > 160 {
        format!("{}…", args_str.chars().take(160).collect::<String>())
    } else {
        args_str
    };

    let lines = vec![
        Line::from(vec![
            Span::raw(" TOOL CALL ").bold().white().on_red(),
            Span::raw("  "),
            Span::raw(format!("{} ", approval.tool_name)).bold().white(),
            Span::raw(format!("[{}] ", approval.risk.label()))
                .bold()
                .fg(risk_color),
            Span::raw(format!(
                "#{}  ({} pending)",
                approval.tool_index,
                approvals.len()
            ))
            .dim(),
        ]),
        Line::from(vec![
            Span::raw(" args: ").dim(),
            Span::raw(args_preview).fg(t.fg),
        ]),
        Line::from(vec![
            Span::raw(" [a] approve  ").fg(t.success),
            Span::raw("[d] deny  ").fg(t.error),
            Span::raw("[Esc] dismiss all").fg(t.warning),
        ]),
    ];
    let para = Paragraph::new(lines).block(
        Block::default()
            .title(" Approval ")
            .borders(Borders::ALL)
            .border_style(ratatui::style::Style::default().fg(risk_color)),
    );
    f.render_widget(para, area);
}

fn render_input(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let (border_color, mode_label) = match app.mode {
        AppMode::Chat => (t.success, "❯ "),
        AppMode::Command => (t.warning, ": "),
        AppMode::Approval => (t.error, "! "),
        AppMode::Error => (t.error, "!!"),
    };

    let input = Paragraph::new(Line::from(vec![
        Span::raw(mode_label).bold().fg(border_color),
        Span::raw(&app.current_input).fg(t.fg),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(ratatui::style::Style::default().fg(border_color)),
    );
    f.render_widget(input, area);

    if app.current_input.is_empty() && app.mode == AppMode::Chat {
        let hint = Paragraph::new(Line::from(vec![Span::raw(
            "   type a prompt · / commands · : config",
        )
        .dim()]));
        f.render_widget(
            hint,
            Rect {
                x: area.x + 3,
                y: area.y + 1,
                width: area.width.saturating_sub(4),
                height: 1,
            },
        );
    }

    // place the real terminal cursor after the 2-char mode label
    let cx = area.x + 1 + 2 + app.cursor.min(area.width.saturating_sub(4) as usize) as u16;
    f.set_cursor_position((cx, area.y + 1));
}

fn render_status_bar(f: &mut Frame, area: Rect, app: &App, cfg: &TuiConfig) {
    let t = &cfg.theme;
    let (conn_label, conn_color) = if app.connected {
        ("online", t.success)
    } else {
        ("offline", t.error)
    };
    let (mode_str, mode_color) = match app.mode {
        AppMode::Chat => ("chat", t.success),
        AppMode::Command => ("cmd", t.warning),
        AppMode::Approval => ("approval", t.error),
        AppMode::Error => ("error", t.error),
    };
    let typing = if app.is_typing {
        Span::raw(" ● streaming").bold().fg(t.warning)
    } else {
        Span::raw(" ○ idle").dim()
    };

    let row1 = Line::from(vec![
        Span::raw(" ali").bold().fg(t.accent),
        Span::raw("-kernel ").bold(),
        Span::raw(format!("v{} ", cfg.identity_version)).dim(),
        Span::raw("│ ").dim(),
        Span::raw(&cfg.backend_model).fg(t.accent),
        Span::raw(format!(" @ :{} ", cfg.llama_port)).dim(),
        Span::raw("│ gw ").dim(),
        Span::raw(conn_label).fg(conn_color),
        Span::raw(" │ mode ").dim(),
        Span::raw(mode_str).bold().fg(mode_color),
        typing,
    ]);
    let row2 = Line::from(vec![
        Span::raw(" L5.5 ")
            .bold()
            .fg(ratatui::style::Color::Magenta),
        Span::raw(format!(
            "saved {} chars in {} squeezes",
            human_chars(app.saved_chars),
            app.compress_events
        ))
        .fg(t.accent2),
        Span::raw(" │ route ").dim(),
        Span::raw(app.pipeline.l3_route.as_deref().unwrap_or("—")).fg(t.accent),
    ]);

    let para =
        Paragraph::new(vec![row1, row2]).style(ratatui::style::Style::default().bg(t.bg).fg(t.fg));
    f.render_widget(para, area);
}

fn render_footer(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let line = if let Some(msg) = &app.status_msg {
        Line::from(vec![Span::raw(format!(" {}", msg)).fg(t.warning)])
    } else {
        Line::from(vec![Span::raw(
            " Enter send · ↑↓ history · PgUp/PgDn scroll · Ctrl+Y copy mode · : config · Ctrl+C quit",
        )
        .dim()])
    };
    f.render_widget(Paragraph::new(line), area);
}

fn ui(f: &mut Frame, app: &App, cfg: &TuiConfig) {
    let approval_h = if app.mode == AppMode::Approval && !app.pending_approvals.is_empty() {
        5
    } else {
        0
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),          // status bar
            Constraint::Min(5),             // main
            Constraint::Length(approval_h), // approval card
            Constraint::Length(3),          // input
            Constraint::Length(1),          // footer
        ])
        .split(f.area());

    render_status_bar(f, chunks[0], app, cfg);

    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(chunks[1]);

    match app.mode {
        AppMode::Command => {
            render_command_help(f, main_chunks[0], &cfg.theme);
            render_config_panel(f, main_chunks[1], cfg, &cfg.theme);
        }
        _ => {
            render_transcript(f, main_chunks[0], app, &cfg.theme);
            let right = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(9),
                    Constraint::Length(5),
                    Constraint::Min(3),
                ])
                .split(main_chunks[1]);
            render_kernel_panel(f, right[0], app, &cfg.theme);
            render_savings_panel(f, right[1], app, &cfg.theme);
            render_live_feed(f, right[2], app, &cfg.theme);
        }
    }

    if approval_h > 0 {
        render_approval_card(f, chunks[2], &app.pending_approvals, &cfg.theme);
    }
    render_input(f, chunks[3], app, &cfg.theme);
    render_footer(f, chunks[4], app, &cfg.theme);
}

// --- commands ---

enum CmdResult {
    Out(String),
    Silent,
    Quit,
}

fn handle_slash_command(cmd: &str, _args: &[String], app: &mut App) -> CmdResult {
    match cmd {
        "/help" => CmdResult::Out(
            "slash commands:\n\
             /help   — this list\n\
             /clear  — reset conversation history\n\
             /retry  — re-attempt last request\n\
             /tools  — list available tools\n\
             /status — system status\n\
             /exit   — quit"
                .into(),
        ),
        "/clear" => {
            app.messages.clear();
            app.reset_pipeline();
            app.streaming_content.clear();
            app.status_msg = Some("Conversation cleared.".into());
            CmdResult::Silent
        }
        "/retry" => {
            let last_user_content = app
                .messages
                .iter()
                .rev()
                .find(|m| m.role == "user")
                .map(|m| m.content.clone());
            if let Some(content) = last_user_content {
                app.messages
                    .retain(|m| !(m.role == "user" && m.content == content));
                app.set_input(content);
                app.status_msg = Some("Recalled last request — Enter to resend.".into());
                CmdResult::Silent
            } else {
                CmdResult::Out("Nothing to retry.".into())
            }
        }
        "/tools" => CmdResult::Out(
            "available tools:\n\
             read_file(path)        [LOW]  — read file contents\n\
             write_file(path,c)     [MED]  — write file\n\
             bash_exec(cmd)         [MED]  — run shell command\n\
             crate_search(query)    [LOW]  — search crates.io\n\
             docs_lookup(crate)     [LOW]  — crate documentation\n\
             cargo_new/add/build/test/clippy [HIGH]"
                .into(),
        ),
        "/status" => CmdResult::Out(
            "gateway :9000 · llama :8081 · model ornith-9b-q4_k_m\n\
             config ~/.config/ali/config.toml — run :status for live details"
                .into(),
        ),
        "/exit" | "/quit" => CmdResult::Quit,
        _ => CmdResult::Out(format!("Unknown command: {}. Type /help.", cmd)),
    }
}

fn parse_command(input: &str) -> (String, Vec<String>) {
    let input = input.trim();
    let rest = input
        .strip_prefix(':')
        .or_else(|| input.strip_prefix('/'))
        .unwrap_or(input);
    let parts: Vec<String> = rest.split_whitespace().map(String::from).collect();
    let cmd = parts.first().cloned().unwrap_or_default();
    let args = parts.get(1..).unwrap_or(&[]).to_vec();
    (cmd, args)
}

async fn handle_command(cmd: &str, args: &[String], cfg: &mut TuiConfig) -> CmdResult {
    match (cmd, args) {
        ("set", [key, value, ..]) => {
            if cfg.set(key, value).is_some() {
                CmdResult::Out(format!("{} = {} (session)", key, value))
            } else {
                CmdResult::Out(format!("Unknown config key: {}", key))
            }
        }
        ("setg", [key, value, ..]) => {
            if cfg.set(key, value).is_some() {
                match cfg.save().await {
                    Ok(p) => CmdResult::Out(format!("{} = {} (saved to {:?})", key, value, p)),
                    Err(e) => CmdResult::Out(format!("{} = {} (save failed: {})", key, value, e)),
                }
            } else {
                CmdResult::Out(format!("Unknown config key: {}", key))
            }
        }
        ("get", [key]) => CmdResult::Out(match key.as_str() {
            "gateway_port" => format!("gateway_port = {}", cfg.gateway_port),
            "llama_port" => format!("llama_port = {}", cfg.llama_port),
            "human_gate" | "human_gate_enabled" => {
                format!("human_gate = {}", cfg.human_gate_enabled)
            }
            "backend_url" => format!("backend_url = {}", cfg.backend_url),
            "backend_model" => format!("backend_model = {}", cfg.backend_model),
            "claude_enabled" => format!("claude_enabled = {}", cfg.claude_enabled),
            "claude_model" => format!("claude_model = {}", cfg.claude_model),
            "tui_theme" => format!("tui_theme = {}", cfg.tui_theme),
            "tui_pipeline_panel" => format!("tui_pipeline_panel = {}", cfg.tui_pipeline_panel),
            "identity_version" => format!("version = {}", cfg.identity_version),
            "identity_git_hash" => format!("git_hash = {}", cfg.identity_git_hash),
            _ => format!("Unknown key: {}", key),
        }),
        ("show", []) => CmdResult::Out(
            [
                format!("gateway_port     = {}", cfg.gateway_port),
                format!("llama_port       = {}", cfg.llama_port),
                format!("human_gate       = {}", cfg.human_gate_enabled),
                format!("backend_url      = {}", cfg.backend_url),
                format!("backend_model    = {}", cfg.backend_model),
                format!("claude_enabled   = {}", cfg.claude_enabled),
                format!("claude_model     = {}", cfg.claude_model),
                format!("tui_theme        = {}", cfg.tui_theme),
                format!("tui_pipeline     = {}", cfg.tui_pipeline_panel),
                format!("version          = {}", cfg.identity_version),
            ]
            .join("\n"),
        ),
        ("save", []) => match cfg.save().await {
            Ok(path) => CmdResult::Out(format!("Saved to {:?}", path)),
            Err(e) => CmdResult::Out(format!("Save failed: {}", e)),
        },
        ("load", []) => {
            *cfg = TuiConfig::load().await;
            CmdResult::Out("Reloaded config from ~/.config/ali/config.toml".into())
        }
        ("identity", []) => CmdResult::Out(format!(
            "Ali Kernel v{}\nbuild: {}\ngit: {}",
            cfg.identity_version, cfg.identity_build_ts, cfg.identity_git_hash
        )),
        ("status", []) => CmdResult::Out(format!(
            "gateway: http://{}:{}\nllama: http://127.0.0.1:{}/v1\nclaude: {}",
            cfg.gateway_host,
            cfg.gateway_port,
            cfg.llama_port,
            if cfg.claude_enabled {
                "enabled"
            } else {
                "disabled"
            }
        )),
        ("tools", []) => CmdResult::Out(
            [
                "read_file(path)           — read file contents [LOW]",
                "write_file(path, content) — write file [MED]",
                "bash_exec(command)        — run bash command [MED]",
                "crate_search(query)       — search crates.io [LOW]",
                "docs_lookup(crate)        — crate documentation [LOW]",
                "cargo_new(project)        — create cargo project [HIGH]",
                "cargo_add(project, crate) — add dependency [HIGH]",
                "cargo_build / cargo_test / cargo_clippy [HIGH]",
            ]
            .join("\n"),
        ),
        ("quit", []) | ("exit", []) => CmdResult::Quit,
        _ => {
            if cmd.is_empty() {
                CmdResult::Silent
            } else {
                CmdResult::Out(format!(
                    "Unknown command: :{}\nType :show for config keys.",
                    cmd
                ))
            }
        }
    }
}

// --- log tailer ---

fn resolve_log_path(cfg: &TuiConfig) -> Option<PathBuf> {
    let primary = cfg.log_dir.join("gateway.log");
    if primary.exists() {
        return Some(primary);
    }
    let fallback = dirs::home_dir()?.join("ali-kernel/logs/gateway.log");
    fallback.exists().then_some(fallback)
}

/// Tail gateway.log from its current end, surviving rotation/truncation,
/// and feed complete lines into the live event pipeline.
fn spawn_log_tailer(app: Arc<RwLock<App>>, log_path: PathBuf) {
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
        let mut file = match tokio::fs::File::open(&log_path).await {
            Ok(f) => f,
            Err(_) => return,
        };
        let mut pos = file.seek(SeekFrom::End(0)).await.unwrap_or(0);
        let mut buf = vec![0u8; 16384];
        let mut line_buf = String::new();
        loop {
            match file.read(&mut buf).await {
                Ok(0) => {
                    if let Ok(meta) = tokio::fs::metadata(&log_path).await {
                        if meta.len() < pos {
                            // truncated/rotated — start over from the top
                            pos = 0;
                            let _ = file.seek(SeekFrom::Start(0)).await;
                            line_buf.clear();
                        }
                    }
                    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                }
                Ok(n) => {
                    pos += n as u64;
                    line_buf.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if let Some(last_nl) = line_buf.rfind('\n') {
                        let complete: String = line_buf.drain(..=last_nl).collect();
                        let mut a = app.write().await;
                        for line in complete.lines() {
                            a.ingest_log_line(line);
                        }
                    }
                }
                Err(_) => {
                    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                }
            }
        }
    });
}

// --- chat dispatch ---

fn spawn_chat_request(
    app: Arc<RwLock<App>>,
    gw: GatewayClient,
    msgs: Vec<gateway::ChatMessage>,
    human_gate: bool,
) {
    tokio::spawn(async move {
        let (gw_tx, mut gw_rx) = mpsc::channel::<gateway::GatewayEvent>(100);

        let gw2 = gw.clone();
        let app2 = app.clone();
        tokio::spawn(async move {
            if let Err(e) = gw2.chat(msgs, tools_as_values(), human_gate, gw_tx).await {
                let mut a = app2.write().await;
                a.is_typing = false;
                a.status_msg = Some(format!("gateway error: {}", e));
            }
        });

        while let Some(evt) = gw_rx.recv().await {
            match evt {
                gateway::GatewayEvent::Token(tok) => {
                    let mut a = app.write().await;
                    a.append_token(&tok);
                }
                gateway::GatewayEvent::ToolCallPending {
                    request_id,
                    tool_name,
                    args,
                    risk,
                    tool_index,
                } => {
                    let mut a = app.write().await;
                    a.current_request_id = Some(request_id.clone());
                    a.add_pending_approval(PendingApproval {
                        request_id,
                        tool_index,
                        tool_name,
                        args,
                        risk: RiskLevel::from_str(&risk),
                        modified_args: None,
                    });
                }
                gateway::GatewayEvent::Done => {
                    let mut a = app.write().await;
                    a.finalize_streaming();
                    a.clear_pending_approvals();
                    break;
                }
                gateway::GatewayEvent::Error(e) => {
                    let mut a = app.write().await;
                    a.finalize_streaming();
                    a.status_msg = Some(format!("Error: {}", e));
                    break;
                }
            }
        }
    });
}

// --- main loop ---

async fn run_tui(app: Arc<RwLock<App>>, gw: GatewayClient, mut cfg: TuiConfig) -> Result<()> {
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
    )?;
    enable_raw_mode()?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    if let Some(log_path) = resolve_log_path(&cfg) {
        spawn_log_tailer(app.clone(), log_path);
    } else {
        let mut a = app.write().await;
        a.status_msg = Some("gateway.log not found — live feed disabled".into());
    }

    // Poll the gateway usage ledger for real billed-usage savings
    {
        let app = app.clone();
        let gw = gw.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(tokio::time::Duration::from_secs(10));
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                iv.tick().await;
                if let Ok(v) = gw.get_stats().await {
                    app.write().await.savings = Some(SavingsStats::from_value(&v));
                }
            }
        });
    }

    let mut mouse_captured = true;

    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(tokio::time::Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    'outer: loop {
        {
            let app_guard = app.read().await;
            terminal.draw(|f| ui(f, &app_guard, &cfg))?;
        }

        let event = tokio::select! {
            maybe = events.next() => match maybe {
                Some(Ok(ev)) => Some(ev),
                Some(Err(_)) | None => break 'outer,
            },
            _ = tick.tick() => None,
        };

        let Some(event) = event else { continue };

        match event {
            Event::Paste(text) => {
                let mut a = app.write().await;
                if a.mode != AppMode::Approval {
                    a.insert_str(&text.replace('\n', " "));
                }
            }
            Event::Mouse(m) => {
                let mut a = app.write().await;
                match m.kind {
                    MouseEventKind::ScrollUp => a.scroll_offset += 3,
                    MouseEventKind::ScrollDown => {
                        a.scroll_offset = a.scroll_offset.saturating_sub(3)
                    }
                    _ => {}
                }
            }
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                let mut a = app.write().await;
                // any keystroke dismisses a stale one-line status
                if !matches!(key.code, KeyCode::Enter) {
                    a.status_msg = None;
                }

                // global bindings
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    match key.code {
                        KeyCode::Char('c') => break 'outer,
                        KeyCode::Char('l') => {
                            a.messages.clear();
                            a.streaming_content.clear();
                            a.reset_pipeline();
                            continue;
                        }
                        KeyCode::Char('d') => {
                            if a.mode == AppMode::Command {
                                a.mode = AppMode::Chat;
                                a.current_input.clear();
                                a.cursor = 0;
                            } else if a.current_input.is_empty() {
                                break 'outer;
                            }
                            continue;
                        }
                        KeyCode::Char('y') => {
                            // Mouse capture eats click-drag, which blocks the
                            // terminal's native text selection — toggle it off
                            // so the user can select and copy.
                            mouse_captured = !mouse_captured;
                            let mut out = io::stdout();
                            if mouse_captured {
                                let _ = crossterm::execute!(out, EnableMouseCapture);
                                a.status_msg = Some(
                                    "mouse capture ON — wheel scrolls · Ctrl+Y to select/copy text"
                                        .into(),
                                );
                            } else {
                                let _ = crossterm::execute!(out, DisableMouseCapture);
                                a.status_msg = Some(
                                    "COPY MODE — select text with mouse, then copy · Ctrl+Y restores wheel scroll"
                                        .into(),
                                );
                            }
                            continue;
                        }
                        _ => {}
                    }
                }

                // approval mode owns its keys
                if a.mode == AppMode::Approval {
                    match key.code {
                        KeyCode::Char('a') => {
                            if let Some(approval) = a.pending_approvals.first().cloned() {
                                a.pending_approvals.remove(0);
                                if a.pending_approvals.is_empty() {
                                    a.mode = AppMode::Chat;
                                }
                                let gw = gw.clone();
                                tokio::spawn(async move {
                                    let _ = gw
                                        .approve_tool(
                                            &approval.request_id,
                                            approval.tool_index,
                                            None,
                                        )
                                        .await;
                                });
                            }
                        }
                        KeyCode::Char('d') => {
                            if let Some(approval) = a.pending_approvals.first().cloned() {
                                a.pending_approvals.remove(0);
                                if a.pending_approvals.is_empty() {
                                    a.mode = AppMode::Chat;
                                }
                                let gw = gw.clone();
                                tokio::spawn(async move {
                                    let _ = gw
                                        .deny_tool(&approval.request_id, approval.tool_index)
                                        .await;
                                });
                            }
                        }
                        KeyCode::Esc => a.clear_pending_approvals(),
                        _ => {}
                    }
                    continue;
                }

                match key.code {
                    KeyCode::Enter => {
                        let input = a.take_input();
                        let trimmed = input.trim().to_string();
                        a.mode = AppMode::Chat;
                        if trimmed.is_empty() {
                            continue;
                        }
                        a.input_history.push(trimmed.clone());

                        if let Some(stripped) = trimmed.strip_prefix(':') {
                            let (cmd, args) = parse_command(stripped);
                            match handle_command(&cmd, &args, &mut cfg).await {
                                CmdResult::Out(out) => a.push_message("system", out),
                                CmdResult::Silent => {}
                                CmdResult::Quit => break 'outer,
                            }
                        } else if trimmed.starts_with('/') {
                            let (cmd, args) = parse_command(&trimmed);
                            match handle_slash_command(&format!("/{}", cmd), &args, &mut a) {
                                CmdResult::Out(out) => a.push_message("system", out),
                                CmdResult::Silent => {}
                                CmdResult::Quit => break 'outer,
                            }
                        } else {
                            a.finalize_streaming();
                            a.push_message("user", trimmed);
                            a.reset_pipeline();
                            a.scroll_offset = 0;
                            a.is_typing = true;

                            let msgs: Vec<_> = a
                                .messages
                                .iter()
                                .filter(|m| m.role == "user" || m.role == "assistant")
                                .map(|m| gateway::ChatMessage {
                                    role: m.role.clone(),
                                    content: m.content.clone(),
                                })
                                .collect();
                            spawn_chat_request(
                                app.clone(),
                                gw.clone(),
                                msgs,
                                cfg.human_gate_enabled,
                            );
                        }
                    }
                    KeyCode::Char(':') if a.current_input.is_empty() && a.mode == AppMode::Chat => {
                        a.insert_char(':');
                        a.mode = AppMode::Command;
                    }
                    KeyCode::Char(c) => a.insert_char(c),
                    KeyCode::Backspace => {
                        a.backspace();
                        if a.mode == AppMode::Command && a.current_input.is_empty() {
                            a.mode = AppMode::Chat;
                        }
                    }
                    KeyCode::Delete => a.delete_forward(),
                    KeyCode::Left => a.cursor_left(),
                    KeyCode::Right => a.cursor_right(),
                    KeyCode::Home => a.cursor_home(),
                    KeyCode::Up => a.history_prev(),
                    KeyCode::Down => a.history_next(),
                    KeyCode::PageUp => a.scroll_offset += SCROLL_PAGE,
                    KeyCode::PageDown => {
                        a.scroll_offset = a.scroll_offset.saturating_sub(SCROLL_PAGE)
                    }
                    KeyCode::End => {
                        if a.current_input.is_empty() {
                            a.scroll_offset = 0;
                        } else {
                            a.cursor_end();
                        }
                    }
                    KeyCode::Esc => {
                        a.mode = AppMode::Chat;
                        a.current_input.clear();
                        a.cursor = 0;
                        a.scroll_offset = 0;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableBracketedPaste,
        DisableMouseCapture
    )?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = TuiConfig::load().await;
    let gateway_url = cfg.gateway_url();
    std::env::set_var("ALI_GATEWAY_URL", &gateway_url);

    let gw = GatewayClient::new();

    let app = Arc::new(RwLock::new(App::new()));
    {
        let mut a = app.write().await;
        a.connected = gw.health_check().await.is_ok();
        a.status_msg = if a.connected {
            None
        } else {
            Some("gateway unreachable — start ali-kernel for full experience".into())
        };
    }

    if let Err(e) = run_tui(app, gw, cfg).await {
        eprintln!("TUI error: {}", e);
    }

    Ok(())
}
