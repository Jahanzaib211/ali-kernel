mod app;
mod gateway;

use anyhow::Result;
use app::{App, AppMode, PendingApproval, RiskLevel};
use crossterm::{
    event::{DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame, Terminal,
};
use std::io;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use gateway::GatewayClient;

fn tools_as_values() -> Vec<serde_json::Value> {
    let tools = [
        r#"{"type":"function","function":{"name":"read_file","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}}}"#,
        r#"{"type":"function","function":{"name":"bash_exec","parameters":{"type":"object","properties":{"command":{"type":"string"}},"required":["command"]}}}"#,
        r#"{"type":"function","function":{"name":"crate_search","parameters":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}}}"#,
        r#"{"type":"function","function":{"name":"docs_lookup","parameters":{"type":"object","properties":{"crate":{"type":"string"}},"required":["crate"]}}}"#,
    ];
    tools.iter().filter_map(|t| serde_json::from_str(t).ok()).collect()
}

fn render_chat(f: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = app
        .messages
        .iter()
        .map(|msg| {
            let role_color = match msg.role.as_str() {
                "user" => Color::Cyan,
                "assistant" => Color::Green,
                "tool" => Color::Yellow,
                _ => Color::White,
            };
            let prefix = format!("[{}] ", msg.role.chars().next().unwrap_or('?'));
            let content = if msg.content.len() > 200 {
                format!("{}...", &msg.content[..200])
            } else {
                msg.content.clone()
            };
            let styled_prefix = Span::raw(prefix).bold().fg(role_color);
            ListItem::new(Line::from(vec![styled_prefix, Span::raw(content)]))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .title(" Chat ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );

    let mut list_state = ratatui::widgets::ListState::default();
    if !app.messages.is_empty() {
        list_state.select(Some(app.messages.len() - 1));
    }

    f.render_stateful_widget(list, area, &mut list_state);
}

fn render_input(f: &mut Frame, area: Rect, app: &App) {
    let border_color = if app.mode == AppMode::Chat {
        Color::Green
    } else {
        Color::DarkGray
    };
    let input = Paragraph::new(app.current_input.as_str())
        .block(
            Block::default()
                .title(" Input ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color)),
        )
        .style(Style::default().fg(Color::White));
    f.render_widget(input, area);
}

fn render_pipeline(f: &mut Frame, area: Rect, app: &App) {
    let dash = "—";
    let l3 = app.pipeline.l3_route.clone().unwrap_or_else(|| dash.to_string());
    let l4 = app.pipeline.l4_memories.map(|n| n.to_string()).unwrap_or_else(|| dash.to_string());
    let l5 = app.pipeline.l5_messages.map(|n| n.to_string()).unwrap_or_else(|| dash.to_string());
    let l7 = app.pipeline.l7_tool.clone().unwrap_or_else(|| dash.to_string());
    let l8 = format!("{:?}", app.pipeline.l8_critique);
    let l9 = app.pipeline.l9_tokens.clone().unwrap_or_else(|| dash.to_string());

    let lines = vec![
        Line::from(vec![Span::raw(" L3 Route:   "), Span::raw(&l3).cyan()]),
        Line::from(vec![Span::raw(" L4 Memory:  "), Span::raw(&l4).cyan()]),
        Line::from(vec![Span::raw(" L5 Msgs:    "), Span::raw(&l5).cyan()]),
        Line::from(vec![Span::raw(" L7 Tool:    "), Span::raw(&l7).yellow()]),
        Line::from(vec![Span::raw(" L8 Critique:"), Span::raw(&l8).magenta()]),
        Line::from(vec![Span::raw(" L9 Tokens:  "), Span::raw(&l9).blue()]),
    ];

    let para = Paragraph::new(lines)
        .block(
            Block::default()
                .title(" Pipeline ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
    f.render_widget(para, area);
}

fn render_approval_card(f: &mut Frame, area: Rect, approvals: &[PendingApproval]) {
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
        Span::raw("[a] Approve  ").green(),
        Span::raw("[d] Deny  ").red(),
        Span::raw("[m] Modify  ").yellow(),
        Span::raw("[Enter] send  ").cyan(),
    ]);

    let lines = vec![
        header,
        Line::from(""),
        Line::from(vec![Span::raw(" Args: ").bold(), Span::raw(&args_preview).white()]),
        Line::from(""),
        help,
    ];

    let para = Paragraph::new(lines)
        .block(
            Block::default()
                .title(" Approval #1 ".to_string())
                .borders(Borders::ALL)
                .border_style(Style::default().fg(risk_color)),
        );
    f.render_widget(para, area);
}

fn render_status_bar(f: &mut Frame, area: Rect, app: &App) {
    let conn_color = if app.connected { Color::Green } else { Color::Red };
    let status = if app.connected { "connected" } else { "disconnected" };
    let typing = if app.is_typing { "typing..." } else { "" };

    let spans = vec![
        Span::raw(" Ali Kernel  ").bold().white(),
        Span::raw("  │  ").dim(),
        Span::raw(format!("gateway:{} ", status)).fg(conn_color),
        Span::raw("  │  ").dim(),
        Span::raw(format!("{} ", typing)).italic().yellow(),
    ];

    let para = Paragraph::new(Line::from(spans))
        .style(Style::default().bg(Color::Black).fg(Color::White));
    f.render_widget(para, area);
}

fn render_current_message(f: &mut Frame, area: Rect, app: &App) {
    if app.streaming_content.is_empty() {
        return;
    }
    let content = if app.streaming_content.len() > 300 {
        format!("{}...", &app.streaming_content[..300])
    } else {
        app.streaming_content.clone()
    };

    let para = Paragraph::new(Line::from(vec![
        Span::raw("[a] ").green(),
        Span::raw(&content).white(),
    ]))
    .block(
        Block::default()
            .title(" Assistant (streaming...) ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(para, area);
}

fn ui(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(8),
            Constraint::Length(3),
        ])
        .split(f.area());

    let chat_area = chunks[1];
    let msg_area = chunks[2];
    let input_area = chunks[3];

    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(1), Constraint::Percentage(30)])
        .split(chat_area);

    render_status_bar(f, chunks[0], app);
    render_chat(f, main_chunks[0], app);
    render_pipeline(f, main_chunks[1], app);

    match app.mode {
        AppMode::Approval => render_approval_card(f, msg_area, &app.pending_approvals),
        AppMode::Chat | AppMode::Error => {
            render_current_message(f, msg_area, app);
        }
    }

    render_input(f, input_area, app);
}

async fn run_tui(app: Arc<RwLock<App>>, gw: GatewayClient) -> Result<()> {
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    enable_raw_mode()?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Spawn gateway log tailer
    let app_for_tailer = app.clone();
    tokio::spawn(async move {
        let log_path = std::path::PathBuf::from("logs/gateway.log");
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
                                if line.starts_with("PIPE ") {
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
        terminal.draw(|f| {
            let app_guard = app.blocking_read();
            ui(f, &app_guard);
        })?;

        if crossterm::event::poll(std::time::Duration::from_millis(50))? {
            let event = crossterm::event::read()?;
            if let Event::Key(key) = event {
                let mut app_guard = app.write().await;

                match key.code {
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
                    KeyCode::Char('c')
                        if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) =>
                    {
                        break;
                    }
                    KeyCode::Enter => {
                        if app_guard.mode == AppMode::Chat && !app_guard.current_input.is_empty() {
                            let input = app_guard.current_input.clone();
                            app_guard.input_history.push(input.clone());
                            app_guard.current_input.clear();
                            app_guard.finalize_streaming();
                            app_guard.push_message("user", input.clone());
                            app_guard.reset_pipeline();

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
                                        .chat(msgs, tools_as_values(), true, gw_tx)
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
                                        else => break,
                                    }
                                }
                            });
                        }
                    }
                    KeyCode::Char(c) => {
                        if app_guard.mode == AppMode::Chat {
                            app_guard.current_input.push(c);
                        }
                    }
                    KeyCode::Backspace => {
                        if app_guard.mode == AppMode::Chat {
                            app_guard.current_input.pop();
                        }
                    }
                    KeyCode::Esc => {
                        app_guard.clear_pending_approvals();
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

    if let Err(e) = run_tui(app, gw).await {
        eprintln!("TUI error: {}", e);
    }

    Ok(())
}
