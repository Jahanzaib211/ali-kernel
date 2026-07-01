mod app;
mod config;
mod gateway;

use anyhow::Result;
use app::{App, AppMode, PendingApproval, RiskLevel};
use config::{TuiConfig, Theme};
use crossterm::{
    event::{DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::Stylize,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame, Terminal,
};
use serde_json::Value;
use std::io;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use gateway::GatewayClient;

fn tools_as_values() -> Vec<Value> {
    let tools = [
        r#"{"type":"function","function":{"name":"read_file","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}}}"#,
        r#"{"type":"function","function":{"name":"bash_exec","parameters":{"type":"object","properties":{"command":{"type":"string"}},"required":["command"]}}}"#,
        r#"{"type":"function","function":{"name":"crate_search","parameters":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}}}"#,
        r#"{"type":"function","function":{"name":"docs_lookup","parameters":{"type":"object","properties":{"crate":{"type":"string"}},"required":["crate"]}}}"#,
    ];
    tools.iter().filter_map(|t| serde_json::from_str(t).ok()).collect()
}

fn render_chat(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let items: Vec<ListItem> = app
        .messages
        .iter()
        .map(|msg| {
            let role_color = match msg.role.as_str() {
                "user" => t.user_msg,
                "assistant" => t.assistant_msg,
                "tool" => t.tool_msg,
                _ => t.fg,
            };
            let prefix = format!("[{}] ", msg.role.chars().next().unwrap_or('?'));
            let content = if msg.content.len() > 200 {
                format!("{}...", &msg.content[..200])
            } else {
                msg.content.clone()
            };
            let styled_prefix = Span::raw(prefix).bold().fg(role_color);
            ListItem::new(Line::from(vec![styled_prefix, Span::raw(content).fg(t.fg)]))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .title(" Chat ")
                .borders(Borders::ALL)
                .border_style(ratatui::style::Style::default().fg(t.border)),
        );

    let mut list_state = ratatui::widgets::ListState::default();
    if !app.messages.is_empty() {
        list_state.select(Some(app.messages.len() - 1));
    }

    f.render_stateful_widget(list, area, &mut list_state);
}

fn render_input(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let border_color = match app.mode {
        AppMode::Chat => t.success,
        AppMode::Command => t.warning,
        AppMode::Approval => t.error,
        AppMode::Error => t.error,
    };
    let mode_label = match app.mode {
        AppMode::Chat => "> ",
        AppMode::Command => ": ",
        AppMode::Approval => "!/ ",
        AppMode::Error => "!! ",
    };
    let prompt = format!("{}{}", mode_label, app.current_input);
    let prompt_style = border_color;

    let input = Paragraph::new(Line::from(vec![Span::raw(&prompt).fg(prompt_style)]))
        .block(
            Block::default()
                .title(if app.mode == AppMode::Command { " Command (type : to enter commands) " } else { " Input (type / for slash commands) " })
                .borders(Borders::ALL)
                .border_style(ratatui::style::Style::default().fg(border_color)),
        )
        .style(ratatui::style::Style::default().fg(t.fg));
    f.render_widget(input, area);

    // Render hint when empty
    if app.current_input.is_empty() && app.mode == AppMode::Chat {
        let hint = Paragraph::new(Line::from(vec![Span::raw("  Type /help for commands, /clear to reset").dim()]))
            .style(ratatui::style::Style::default().bg(t.bg).fg(t.muted));
        f.render_widget(hint, area);
    }
}

fn render_pipeline(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    let dash = "—";
    let route = app.pipeline.l3_route.clone().unwrap_or_else(|| dash.to_string());
    let memories = app.pipeline.l4_memories.map(|n| n.to_string()).unwrap_or_else(|| dash.to_string());
    let messages = app.pipeline.l5_messages.map(|n| n.to_string()).unwrap_or_else(|| dash.to_string());
    let tool = app.pipeline.l7_tool.clone().unwrap_or_else(|| dash.to_string());
    let critique = app.pipeline.l8_critique.unwrap_or(false);
    let critique_str = if critique { "yes" } else { "no" };
    let critique_color = if critique { t.success } else { t.muted };
    let tokens = app.pipeline.l9_tokens.clone().unwrap_or_else(|| dash.to_string());
    let compression = app.pipeline.l5_5_compression.clone().unwrap_or_else(|| dash.to_string());

    // System info block
    let sys_lines = vec![
        Line::from(vec![Span::raw(" SYSTEM ").bold().white().on_black()]),
        Line::from(vec![Span::raw(" model:    ").dim(), Span::raw("ornith-9b").fg(t.accent)]),
        Line::from(vec![Span::raw(" route:    ").dim(), Span::raw(&route).fg(t.accent)]),
        Line::from(vec![Span::raw(" memories: ").dim(), Span::raw(&memories).fg(t.accent)]),
        Line::from(vec![Span::raw(" messages: ").dim(), Span::raw(&messages).fg(t.accent)]),
        Line::from(vec![Span::raw(" tokens:   ").dim(), Span::raw(&tokens).fg(t.accent2)]),
        Line::from(vec![Span::raw(" compress: ").dim(), Span::raw(&compression).fg(t.accent2)]),
    ];

    let sys_block = Paragraph::new(sys_lines)
        .block(
            Block::default()
                .title(" Kernel ")
                .borders(Borders::ALL)
                .border_style(ratatui::style::Style::default().fg(t.border)),
        );

    // Tool block
    let tool_lines = vec![
        Line::from(vec![Span::raw(" TOOL ").bold().white().on_black()]),
        Line::from(vec![Span::raw(" last:  ").dim()]),
        Line::from(vec![Span::raw(&tool).fg(t.warning).bold()]),
    ];
    let tool_block = Paragraph::new(tool_lines)
        .block(
            Block::default()
                .title(" Execution ")
                .borders(Borders::ALL)
                .border_style(ratatui::style::Style::default().fg(t.border)),
        );

    // Critique block
    let crit_lines = vec![
        Line::from(vec![Span::raw(" CRITIQUE ").bold().white().on_black()]),
        Line::from(vec![Span::raw(" applied: ").dim(), Span::raw(critique_str).fg(critique_color).bold()]),
    ];
    let crit_block = Paragraph::new(crit_lines)
        .block(
            Block::default()
                .title(" Self-Review ")
                .borders(Borders::ALL)
                .border_style(ratatui::style::Style::default().fg(t.border)),
        );

    // Stack them vertically
    let v_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Length(1), Constraint::Percentage(50)])
        .split(area);

    let h_chunks_top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Min(1)])
        .split(v_chunks[0]);

    let h_chunks_bot = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Min(1)])
        .split(v_chunks[2]);

    f.render_widget(sys_block, h_chunks_top[0]);
    f.render_widget(tool_block, h_chunks_top[1]);
    f.render_widget(crit_block, h_chunks_bot[0]);
    f.render_widget(
        Paragraph::new(vec![Line::from(""), Line::from(" L7: TODO"), Line::from(" L8: TODO"), Line::from(" L9: TODO")])
            .block(Block::default().title(" Layers ").borders(Borders::ALL).border_style(ratatui::style::Style::default().fg(t.border))),
        h_chunks_bot[1],
    );
}

fn render_config_panel(f: &mut Frame, area: Rect, cfg: &TuiConfig, t: &Theme) {
    let lines = vec![
        Line::from(vec![Span::raw(" Config Stack ").bold().white()]),
        Line::from(vec![Span::raw("─────────────────────────────────").dim()]),
        Line::from(vec![Span::raw(" gateway_port:  "), Span::raw(format!("{}", cfg.gateway_port)).fg(t.accent)]),
        Line::from(vec![Span::raw(" llama_port:    "), Span::raw(format!("{}", cfg.llama_port)).fg(t.accent)]),
        Line::from(vec![Span::raw(" human_gate:    "), Span::raw(format!("{}", cfg.human_gate_enabled)).fg(t.warning)]),
        Line::from(vec![Span::raw(" backend_url:    "), Span::raw(&cfg.backend_url).fg(t.accent)]),
        Line::from(vec![Span::raw(" backend_model:  "), Span::raw(&cfg.backend_model).fg(t.accent)]),
        Line::from(vec![Span::raw(" claude_enabled: "), Span::raw(format!("{}", cfg.claude_enabled)).fg(t.warning)]),
        Line::from(vec![Span::raw(" tui_theme:     "), Span::raw(&cfg.tui_theme).fg(t.success)]),
        Line::from(vec![Span::raw("─────────────────────────────────").dim()]),
        Line::from(vec![Span::raw(" identity_version: ").dim(), Span::raw(&cfg.identity_version).dim()]),
    ];

    let para = Paragraph::new(lines)
        .block(
            Block::default()
                .title(" Config ")
                .borders(Borders::ALL)
                .border_style(ratatui::style::Style::default().fg(t.border)),
        );
    f.render_widget(para, area);
}

fn render_command_help(f: &mut Frame, area: Rect, t: &Theme) {
    let slash_lines = vec![
        Line::from(vec![Span::raw(" SLASH COMMANDS ").bold().white().on_black()]),
        Line::from(vec![Span::raw("/help   ").cyan().bold(), Span::raw("— list capabilities").dim()]),
        Line::from(vec![Span::raw("/clear  ").cyan().bold(), Span::raw("— reset conversation").dim()]),
        Line::from(vec![Span::raw("/retry  ").cyan().bold(), Span::raw("— re-attempt last request").dim()]),
        Line::from(vec![Span::raw("/tools  ").cyan().bold(), Span::raw("— show available tools").dim()]),
        Line::from(vec![Span::raw("/status ").cyan().bold(), Span::raw("— system status").dim()]),
        Line::from(vec![Span::raw("/exit   ").cyan().bold(), Span::raw("— quit").dim()]),
        Line::from(vec![Span::raw("").dim()]),
        Line::from(vec![Span::raw(" COLON COMMANDS ").bold().white().on_black()]),
        Line::from(vec![Span::raw(":set <k> <v>  ").fg(t.success), Span::raw("set config (session)").dim()]),
        Line::from(vec![Span::raw(":setg <k> <v> ").fg(t.success), Span::raw("set + save to disk").dim()]),
        Line::from(vec![Span::raw(":get <key>     ").fg(t.accent), Span::raw("show config value").dim()]),
        Line::from(vec![Span::raw(":show           ").fg(t.accent), Span::raw("show all config").dim()]),
        Line::from(vec![Span::raw(":save           ").fg(t.warning), Span::raw("persist config").dim()]),
        Line::from(vec![Span::raw(":load           ").fg(t.warning), Span::raw("reload config").dim()]),
        Line::from(vec![Span::raw(":identity       ").fg(t.accent2), Span::raw("system self-description").dim()]),
        Line::from(vec![Span::raw(":status         ").fg(t.accent2), Span::raw("gateway + llama health").dim()]),
        Line::from(vec![Span::raw(":tools          ").fg(t.accent2), Span::raw("list available tools").dim()]),
        Line::from(vec![Span::raw(":quit / Ctrl+d  ").fg(t.error), Span::raw("exit").dim()]),
    ];

    let para = Paragraph::new(slash_lines)
        .block(
            Block::default()
                .title(" Ali Kernel Commands ")
                .borders(Borders::ALL)
                .border_style(ratatui::style::Style::default().fg(t.border)),
        );
    f.render_widget(para, area);
}

fn render_approval_card(f: &mut Frame, area: Rect, approvals: &[PendingApproval], t: &Theme) {
    if approvals.is_empty() {
        return;
    }

    let approval = &approvals[0];
    let risk_color = approval.risk.color();
    let risk_label = approval.risk.label();
    let args_str = serde_json::to_string_pretty(&approval.args).unwrap_or_default();
    let args_preview = if args_str.len() > 120 {
        format!("{}...", &args_str[..120])
    } else {
        args_str.clone()
    };

    let header = Line::from(vec![
        Span::raw(" TOOL CALL ").bold().white().on_red(),
        Span::raw("  "),
        Span::raw(format!("{} ", approval.tool_name)).bold().white(),
        Span::raw(format!("[{}] ", risk_label)).bold().fg(risk_color),
        Span::raw(format!("#{}", approval.tool_index)).dim(),
    ]);

    let help = Line::from(vec![
        Span::raw("[a] Approve  ").fg(t.success),
        Span::raw("[d] Deny  ").fg(t.error),
        Span::raw("[m] Modify  ").fg(t.warning),
        Span::raw("[Enter] send  ").fg(t.accent),
    ]);

    let lines = vec![
        header,
        Line::from(""),
        Line::from(vec![Span::raw(" Args: ").bold(), Span::raw(&args_preview).fg(t.fg)]),
        Line::from(""),
        help,
    ];

    let para = Paragraph::new(lines)
        .block(
            Block::default()
                .title(" Approval #1 ".to_string())
                .borders(Borders::ALL)
                .border_style(ratatui::style::Style::default().fg(risk_color)),
        );
    f.render_widget(para, area);
}

fn render_status_bar(f: &mut Frame, area: Rect, app: &App, cfg: &TuiConfig) {
    let t = &cfg.theme;
    let conn_color = if app.connected { t.success } else { t.error };
    let conn_label = if app.connected { "connected" } else { "disconnected" };

    let mode_str = match app.mode {
        AppMode::Chat => "chat",
        AppMode::Command => "command",
        AppMode::Approval => "approval",
        AppMode::Error => "error",
    };
    let mode_color = match app.mode {
        AppMode::Chat => t.success,
        AppMode::Command => t.warning,
        AppMode::Approval => t.error,
        AppMode::Error => t.error,
    };

    // Row 1: Identity + version
    let row1 = Line::from(vec![
        Span::raw(" Ali Kernel ").bold().white().fg(t.accent),
        Span::raw(" v").dim(),
        Span::raw(&cfg.identity_version).bold().white(),
        Span::raw(" | model: ").dim(),
        Span::raw(&cfg.backend_model).fg(t.accent).bold(),
        Span::raw(" | backend: ").dim(),
        Span::raw(&cfg.backend_url).dim(),
    ]);

    // Row 2: System status
    let typing_indicator = if app.is_typing {
        Span::raw(" ● streaming ").bold().fg(t.warning)
    } else {
        Span::raw(" ○ idle ").dim()
    };
    let route_info = app.pipeline.l3_route.as_deref().unwrap_or("—");
    let row2 = Line::from(vec![
        Span::raw(" mode: ").dim(),
        Span::raw(mode_str).bold().fg(mode_color),
        Span::raw(" | gateway: ").dim(),
        Span::raw(conn_label).fg(conn_color),
        Span::raw(" | llama: ").dim(),
        Span::raw(format!(":{}", cfg.llama_port)).fg(t.success),
        typing_indicator,
        Span::raw(" | route: ").dim(),
        Span::raw(route_info).fg(t.accent),
    ]);

    let para = Paragraph::new(vec![row1, row2])
        .style(ratatui::style::Style::default().bg(t.bg).fg(t.fg));
    f.render_widget(para, area);
}

fn render_current_message(f: &mut Frame, area: Rect, app: &App, t: &Theme) {
    if app.streaming_content.is_empty() {
        // Show last assistant message instead
        if let Some(last) = app.messages.iter().rev().find(|m| m.role == "assistant") {
            let lines: Vec<Line> = last.content.lines().take(4).map(|l| {
                Line::from(vec![Span::raw("[a] ").fg(t.assistant_msg), Span::raw(l).fg(t.fg)])
            }).collect();
            if !lines.is_empty() {
                let para = Paragraph::new(lines)
                    .block(
                        Block::default()
                            .title(" Last Response ")
                            .borders(Borders::ALL)
                            .border_style(ratatui::style::Style::default().fg(t.border)),
                    );
                f.render_widget(para, area);
            }
        }
        return;
    }

    let content = app.streaming_content.clone();

    let lines: Vec<Line> = content.lines().take(5).map(|l| {
        Line::from(vec![Span::raw("[a] ").fg(t.assistant_msg), Span::raw(l).fg(t.fg)])
    }).collect();

    let streaming_indicator = if app.is_typing { " ● streaming..." } else { "" };
    let para = Paragraph::new(lines)
        .block(
            Block::default()
                .title(format!(" Assistant{}", streaming_indicator))
                .borders(Borders::ALL)
                .border_style(ratatui::style::Style::default().fg(t.border)),
        );
    f.render_widget(para, area);
}

fn ui(f: &mut Frame, app: &App, cfg: &TuiConfig) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // status bar (was 1)
            Constraint::Min(1),    // chat area
            Constraint::Length(1), // separator / current message
            Constraint::Length(4), // input (was 3)
        ])
        .split(f.area());

    let chat_area = chunks[1];
    let msg_area = chunks[2];
    let input_area = chunks[3];

    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
        .split(chat_area);

    render_status_bar(f, chunks[0], app, cfg);

    match app.mode {
        AppMode::Command => {
            render_command_help(f, main_chunks[0], &cfg.theme);
            render_config_panel(f, main_chunks[1], cfg, &cfg.theme);
        }
        _ => {
            render_chat(f, main_chunks[0], app, &cfg.theme);
            render_pipeline(f, main_chunks[1], app, &cfg.theme);
        }
    }

    match app.mode {
        AppMode::Approval => render_approval_card(f, msg_area, &app.pending_approvals, &cfg.theme),
        AppMode::Chat | AppMode::Error => {
            render_current_message(f, msg_area, app, &cfg.theme);
        }
        AppMode::Command => {
            // Command output / status in msg area
            let status_text = app.status_msg.clone().unwrap_or_default();
            let para = Paragraph::new(Line::from(vec![Span::raw(&status_text).fg(cfg.theme.warning)]))
                .block(
                    Block::default()
                        .title(" Output ")
                        .borders(Borders::ALL)
                        .border_style(ratatui::style::Style::default().fg(cfg.theme.border)),
                );
            f.render_widget(para, msg_area);
        }
    }

    render_input(f, input_area, app, &cfg.theme);
}

async fn handle_slash_command(cmd: &str, _args: &[String], app: &mut App) -> Option<String> {
    match cmd {
        "/help" | ":help" => Some(
            "Ali Kernel — available slash commands:\n\
             /help   — this list\n\
             /clear  — reset conversation history\n\
             /retry  — re-attempt last request\n\
             /tools  — list available tools\n\
             /status — system status\n\
             /exit   — quit\n\n\
             Type /model <prompt> to query the model directly.".into(),
        ),
        "/clear" | ":clear" => {
            app.messages.clear();
            app.reset_pipeline();
            app.streaming_content.clear();
            app.status_msg = Some("Conversation cleared.".into());
            None
        }
        "/retry" | ":retry" => {
            // Find the last user message
            let last_user_content = app.messages.iter().rev()
                .find(|m| m.role == "user")
                .map(|m| m.content.clone());
            if let Some(content) = last_user_content {
                app.current_input = content.clone();
                app.messages.retain(|m| !(m.role == "user" && m.content == content));
                app.status_msg = Some("Retrying last request...".into());
                None
            } else {
                Some("Nothing to retry.".into())
            }
        }
        "/tools" | ":tools" => Some(
            "Available tools:\n\
             read_file(path)       [LOW]     — read file contents\n\
             write_file(path,c)   [MED]     — write file\n\
             bash_exec(cmd)       [MED]     — run shell command\n\
             crate_search(query)  [LOW]     — search crates.io\n\
             docs_lookup(crate)   [LOW]     — crate documentation\n\
             cargo_new(project)   [HIGH]    — create cargo project\n\
             cargo_add(proj,dep)  [HIGH]    — add dependency\n\
             cargo_build          [HIGH]    — build project\n\
             cargo_test           [HIGH]    — run tests\n\
             cargo_clippy         [HIGH]    — run clippy".into(),
        ),
        "/status" | ":status" => Some(
            "Ali Kernel — system status:\n\
             gateway:  :9000  (API server)\n\
             llama:    :8081  (local model)\n\
             model:    ornith-9b-q4_k_m\n\
             config:   ~/.config/ali/config.toml\n\
             Run :status for full details.".into(),
        ),
        "/exit" | ":exit" | "/quit" | ":quit" => {
            app.status_msg = Some("Goodbye.".into());
            None
        }
        _ => {
            // Unknown slash command — pass through to model
            Some(format!("Unknown command: {}. Type /help for available commands.", cmd))
        }
    }
}

fn parse_command(input: &str) -> (String, Vec<String>) {
    let input = input.trim();
    let prefix = if input.starts_with(':') {
        (":", 1)
    } else if input.starts_with('/') {
        ("/", 1)
    } else {
        return ("".into(), vec![]);
    };
    let rest = &input[prefix.1..];
    let parts: Vec<String> = rest
        .split_whitespace()
        .map(String::from)
        .collect();
    let cmd = parts.first().cloned().unwrap_or_default();
    let args = parts.get(1..).unwrap_or(&[]).to_vec();
    (format!("{}{}", prefix.0, cmd), args)
}

async fn handle_command(cmd: &str, args: &[String], app: &mut App, cfg: &mut TuiConfig) -> Option<String> {
    match (cmd, args) {
        // :set <key> <value> — ephemeral session config
        ("set", [key, value, ..]) => {
            if cfg.set(key, value).is_some() {
                Some(format!("{} = {} (session)", key, value))
            } else {
                Some(format!("Unknown config key: {}", key))
            }
        }
        // :setg <key> <value> — set + persist
        ("setg", [key, value, ..]) => {
            if cfg.set(key, value).is_some() {
                let path = cfg.save().await;
                match path {
                    Ok(p) => Some(format!("{} = {} (saved to {:?})", key, value, p)),
                    Err(e) => Some(format!("{} = {} (save failed: {})", key, value, e)),
                }
            } else {
                Some(format!("Unknown config key: {}", key))
            }
        }
        // :get <key>
        ("get", [key]) => {
            match key.as_str() {
                "gateway_port" => Some(format!("gateway_port = {}", cfg.gateway_port)),
                "llama_port" => Some(format!("llama_port = {}", cfg.llama_port)),
                "human_gate" | "human_gate_enabled" => {
                    Some(format!("human_gate = {}", cfg.human_gate_enabled))
                }
                "backend_url" => Some(format!("backend_url = {}", cfg.backend_url)),
                "backend_model" => Some(format!("backend_model = {}", cfg.backend_model)),
                "claude_enabled" => Some(format!("claude_enabled = {}", cfg.claude_enabled)),
                "claude_model" => Some(format!("claude_model = {}", cfg.claude_model)),
                "tui_theme" => Some(format!("tui_theme = {}", cfg.tui_theme)),
                "tui_pipeline_panel" => {
                    Some(format!("tui_pipeline_panel = {}", cfg.tui_pipeline_panel))
                }
                "identity_version" => Some(format!("version = {}", cfg.identity_version)),
                "identity_git_hash" => Some(format!("git_hash = {}", cfg.identity_git_hash)),
                _ => Some(format!("Unknown key: {}", key)),
            }
        }
        // :show — all config
        ("show", []) => {
            let lines = vec![
                format!("gateway_port     = {} ← env/ali", cfg.gateway_port),
                format!("llama_port       = {} ← env/ali", cfg.llama_port),
                format!("human_gate       = {} ← env/ali", cfg.human_gate_enabled),
                format!("backend_url      = {} ← env/ali", cfg.backend_url),
                format!("backend_model    = {} ← env/ali", cfg.backend_model),
                format!("claude_enabled   = {} ← default", cfg.claude_enabled),
                format!("claude_model     = {} ← default", cfg.claude_model),
                format!("tui_theme        = {} ← default", cfg.tui_theme),
                format!("tui_pipeline     = {} ← default", cfg.tui_pipeline_panel),
                format!("version          = {} ← build", cfg.identity_version),
            ];
            Some(lines.join("\n"))
        }
        // :save
        ("save", []) => {
            match cfg.save().await {
                Ok(path) => Some(format!("Saved to {:?}", path)),
                Err(e) => Some(format!("Save failed: {}", e)),
            }
        }
        // :load
        ("load", []) => {
            *cfg = TuiConfig::load().await;
            Some("Reloaded config from ~/.config/ali/config.toml".into())
        }
        // :identity
        ("identity", []) => {
            Some(format!(
                "Ali Kernel v{}\nbuild: {}\ngit: {}",
                cfg.identity_version, cfg.identity_build_ts, cfg.identity_git_hash
            ))
        }
        // :status
        ("status", []) => {
            Some(format!(
                "gateway: http://{}:{}\nllama: http://127.0.0.1:{}/v1\nclaude: {}",
                cfg.gateway_host, cfg.gateway_port, cfg.llama_port,
                if cfg.claude_enabled { "enabled" } else { "disabled" }
            ))
        }
        // :tools
        ("tools", []) => {
            Some(
                [
                    "read_file(path)          — read file contents [LOW]",
                    "write_file(path, content) — write file [MED]",
                    "bash_exec(command)        — run bash command [MED]",
                    "crate_search(query)       — search crates.io [LOW]",
                    "docs_lookup(crate)        — crate documentation [LOW]",
                    "cargo_new(project)        — create cargo project [HIGH]",
                    "cargo_add(project, crate) — add dependency [HIGH]",
                    "cargo_build               — build project [HIGH]",
                    "cargo_test                — run tests [HIGH]",
                    "cargo_clippy              — run clippy [HIGH]",
                ]
                .join("\n"),
            )
        }
        // :quit
        ("quit", []) | ("exit", []) => {
            app.status_msg = Some("Goodbye.".into());
            None
        }
        // Unknown
        _ => {
            if cmd.is_empty() {
                None
            } else {
                Some(format!("Unknown command: :{}\nType :show for available commands.", cmd))
            }
        }
    }
}

async fn run_tui(
    app: Arc<RwLock<App>>,
    gw: GatewayClient,
    mut cfg: TuiConfig,
) -> Result<()> {
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    enable_raw_mode()?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Spawn gateway log tailer — reads PIPE lines from kernel's gateway.log
    let log_dir = cfg.log_dir.clone();
    let app_for_tailer = app.clone();
    tokio::spawn(async move {
        let log_path = log_dir.join("gateway.log");
        if !log_path.exists() {
            return;
        }
        if let Ok(mut file) = tokio::fs::File::open(&log_path).await {
            use tokio::io::AsyncReadExt;
            let mut buf = vec![0u8; 8192];
            loop {
                match file.read(&mut buf).await {
                    Ok(0) => {
                        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                    }
                    Ok(n) => {
                        if let Ok(text) = String::from_utf8(buf[..n].to_vec()) {
                            let mut a = app_for_tailer.write().await;
                            for line in text.lines() {
                                if line.starts_with("PIPE ") || line.starts_with("Tool executed:") {
                                    a.update_pipeline(line);
                                }
                            }
                        }
                    }
                    Err(_) => {
                        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                    }
                }
            }
        }
    });

    loop {
        let app_guard = app.read().await;
        let cfg_clone = cfg.clone();
        terminal.draw(|f| {
            ui(f, &app_guard, &cfg_clone);
        })?;
        drop(app_guard);

        if crossterm::event::poll(std::time::Duration::from_millis(50))? {
            let event = crossterm::event::read()?;
            if let Event::Key(key) = event {
                let mut app_guard = app.write().await;

                match key.code {
                    // Enter: submit command or chat
                    KeyCode::Enter => {
                        let input = app_guard.current_input.clone();
                        app_guard.current_input.clear();

                        if input.starts_with(':') {
                            let (cmd, args) = parse_command(&input);
                            if let Some(output) = handle_command(&cmd, &args, &mut app_guard, &mut cfg).await {
                                app_guard.status_msg = Some(output);
                            } else {
                                // Quit command
                                break;
                            }
                            app_guard.mode = AppMode::Chat;
                        } else if input.starts_with('/') {
                            // Slash commands: handled locally for instant response
                            let (slash_cmd, slash_args) = parse_command(&input);
                            let output = handle_slash_command(&slash_cmd, &slash_args, &mut app_guard).await;
                            if let Some(msg) = output {
                                app_guard.status_msg = Some(msg);
                            } else {
                                break; // /exit
                            }
                            app_guard.mode = AppMode::Chat;
                        } else if !input.is_empty() {
                            app_guard.input_history.push(input.clone());
                            app_guard.finalize_streaming();
                            app_guard.push_message("user", input.clone());
                            app_guard.reset_pipeline();
                            app_guard.mode = AppMode::Chat;

                            let msgs: Vec<_> = app_guard
                                .messages
                                .iter()
                                .map(|m| gateway::ChatMessage {
                                    role: m.role.clone(),
                                    content: m.content.clone(),
                                })
                                .collect();

                            let app_clone = app.clone();
                            let gw_clone = gw.clone();

                            tokio::spawn(async move {
                                let (gw_tx, mut gw_rx) = mpsc::channel::<gateway::GatewayEvent>(100);

                                let gw_clone2 = gw_clone.clone();
                                let app_clone2 = app_clone.clone();

                                tokio::spawn(async move {
                                    if let Err(e) = gw_clone2
                                        .chat(msgs, tools_as_values(), cfg.human_gate_enabled, gw_tx)
                                        .await
                                    {
                                        let mut a = app_clone2.write().await;
                                        a.status_msg = Some(format!("gateway error: {}", e));
                                    }
                                });

                                loop {
                                    tokio::select! {
                                        Some(evt) = gw_rx.recv() => {
                                            match evt {
                                                gateway::GatewayEvent::Token(tok) => {
                                                    let mut a = app_clone.write().await;
                                                    a.append_token(&tok);
                                                }
                                                gateway::GatewayEvent::ToolCallPending {
                                                    request_id,
                                                    tool_name,
                                                    args,
                                                    risk,
                                                    tool_index,
                                                } => {
                                                    let mut a = app_clone.write().await;
                                                    a.current_request_id = Some(request_id.clone());
                                                    a.add_pending_approval(PendingApproval {
                                                        request_id,
                                                        tool_index,
                                                        tool_name: tool_name.clone(),
                                                        args,
                                                        risk: RiskLevel::from_str(&risk),
                                                        modified_args: None,
                                                    });
                                                }
                                                gateway::GatewayEvent::Done => {
                                                    let mut a = app_clone.write().await;
                                                    a.finalize_streaming();
                                                    a.clear_pending_approvals();
                                                    break;
                                                }
                                                gateway::GatewayEvent::Error(e) => {
                                                    let mut a = app_clone.write().await;
                                                    a.finalize_streaming();
                                                    a.status_msg = Some(format!("Error: {}", e));
                                                    break;
                                                }
                                            }
                                        }
                                        else => {
                                            let mut a = app_clone.write().await;
                                            a.finalize_streaming();
                                            a.status_msg = Some("Connection closed".into());
                                            break;
                                        }
                                    }
                                }
                            });
                        }
                    }
                    // Colon: enter command mode
                    KeyCode::Char(':') => {
                        if app_guard.mode == AppMode::Chat {
                            app_guard.current_input.push(':');
                            app_guard.mode = AppMode::Command;
                        } else {
                            app_guard.current_input.push(':');
                        }
                    }
                    // Approve in approval mode
                    KeyCode::Char('a') => {
                        if app_guard.mode == AppMode::Approval {
                            if let Some(approval) = app_guard.pending_approvals.first() {
                                let rid = approval.request_id.clone();
                                let idx = approval.tool_index;
                                tokio::spawn({
                                    let gw = gw.clone();
                                    async move {
                                        let _ = gw.approve_tool(&rid, idx, None).await;
                                    }
                                });
                            }
                        }
                    }
                    // Ctrl+C: quit
                    KeyCode::Char('c')
                        if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) =>
                    {
                        break;
                    }
                    // Ctrl+D: exit command mode without saving
                    KeyCode::Char('d')
                        if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) =>
                    {
                        if app_guard.mode == AppMode::Command {
                            app_guard.mode = AppMode::Chat;
                            app_guard.current_input.clear();
                        }
                    }
                    // Escape: exit command mode or clear approvals
                    KeyCode::Esc => {
                        if app_guard.mode == AppMode::Command {
                            app_guard.mode = AppMode::Chat;
                            app_guard.current_input.clear();
                        } else {
                            app_guard.clear_pending_approvals();
                        }
                    }
                    // Deny in approval mode
                    KeyCode::Char('d') => {
                        if app_guard.mode == AppMode::Approval {
                            if let Some(approval) = app_guard.pending_approvals.first() {
                                let rid = approval.request_id.clone();
                                let idx = approval.tool_index;
                                tokio::spawn({
                                    let gw = gw.clone();
                                    async move {
                                        let _ = gw.deny_tool(&rid, idx).await;
                                    }
                                });
                            }
                        }
                    }
                    // Backspace
                    KeyCode::Backspace => {
                        if app_guard.mode != AppMode::Approval {
                            app_guard.current_input.pop();
                        }
                    }
                    // Regular character input
                    KeyCode::Char(c)
                        if app_guard.mode != AppMode::Approval =>
                    {
                        app_guard.current_input.push(c);
                    }
                    _ => {}
                }
            }
        }
    }

    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableBracketedPaste)?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = TuiConfig::load().await;
    let gateway_url = cfg.gateway_url();
    std::env::set_var("ALI_GATEWAY_URL", &gateway_url);

    let gw = GatewayClient::new();

    if let Ok(h) = gw.health_check().await {
        println!("Gateway: {}", h);
    } else {
        eprintln!("Warning: gateway unreachable. Type 'ali' with gateway running for full experience.");
    }

    let app = Arc::new(RwLock::new(App::new()));
    {
        let mut a = app.write().await;
        a.connected = gw.health_check().await.is_ok();
    }

    if let Err(e) = run_tui(app, gw, cfg).await {
        eprintln!("TUI error: {}", e);
    }

    Ok(())
}
