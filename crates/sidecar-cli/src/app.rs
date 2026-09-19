use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use tokio::sync::mpsc;

use pipeline::PipelineEvent;

// Column width for aligned label: value pairs
const COL: usize = 16;

// ── Screens ─────────────────────────────────────────────────

enum Action {
    None,
    StartHost,
    Join(String),
    Quit,
}

// ── Landing state ───────────────────────────────────────────

#[derive(PartialEq)]
enum LandingMode {
    Select,
    JoinInput,
}

struct LandingState {
    selected: usize,
    mode: LandingMode,
    room_code: String,
    error: Option<String>,
}

impl LandingState {
    fn new() -> Self {
        Self {
            selected: 0,
            mode: LandingMode::Select,
            room_code: String::new(),
            error: None,
        }
    }

    fn handle_key(&mut self, key: event::KeyEvent) -> Action {
        if key.kind != KeyEventKind::Press {
            return Action::None;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Action::Quit;
        }
        match self.mode {
            LandingMode::Select => match key.code {
                KeyCode::Char('q') | KeyCode::Char('Q') => Action::Quit,
                KeyCode::Up | KeyCode::Char('k') => {
                    self.selected = self.selected.saturating_sub(1);
                    Action::None
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if self.selected < 1 {
                        self.selected += 1;
                    }
                    Action::None
                }
                KeyCode::Enter => {
                    if self.selected == 0 {
                        Action::StartHost
                    } else {
                        self.mode = LandingMode::JoinInput;
                        Action::None
                    }
                }
                _ => Action::None,
            },
            LandingMode::JoinInput => match key.code {
                KeyCode::Esc => {
                    self.mode = LandingMode::Select;
                    self.room_code.clear();
                    Action::None
                }
                KeyCode::Char('q') if self.room_code.is_empty() => Action::Quit,
                KeyCode::Char(c) => {
                    self.room_code.push(c);
                    Action::None
                }
                KeyCode::Backspace => {
                    self.room_code.pop();
                    Action::None
                }
                KeyCode::Enter => {
                    let code = self.room_code.trim().to_string();
                    if code.is_empty() {
                        Action::None
                    } else {
                        Action::Join(code)
                    }
                }
                _ => Action::None,
            },
        }
    }

    fn render(&self, frame: &mut Frame) {
        let dim = Style::default().fg(Color::DarkGray);
        let key = Style::default().fg(Color::White);
        let items = ["Start session", "Join session"];
        let mut lines: Vec<Line> = Vec::new();

        if let Some(ref err) = self.error {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("  {}", err),
                Style::default().fg(Color::Red),
            )));
        }

        lines.push(Line::from(""));
        for (i, label) in items.iter().enumerate() {
            let is_selected = i == self.selected;
            let marker = if is_selected { "> " } else { "  " };
            let style = if is_selected {
                Style::default()
                    .fg(crate::tui::ACCENT)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            lines.push(Line::from(Span::styled(
                format!("  {}{}", marker, label),
                style,
            )));
        }

        if self.mode == LandingMode::JoinInput {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("  Room code: ", dim),
                Span::styled(&self.room_code, Style::default().fg(Color::White)),
                Span::styled("_", Style::default().fg(Color::DarkGray)),
            ]));
        }

        lines.push(Line::from(""));

        let mut shortcuts = vec![Span::styled("  ", dim)];
        if self.mode == LandingMode::JoinInput {
            shortcuts.extend([
                Span::styled("enter", key),
                Span::styled(" join  ", dim),
                Span::styled("esc", key),
                Span::styled(" back  ", dim),
            ]);
        } else {
            shortcuts.extend([
                Span::styled("\u{2191}\u{2193}", key),
                Span::styled(" navigate  ", dim),
                Span::styled("enter", key),
                Span::styled(" select  ", dim),
            ]);
        }
        shortcuts.extend([Span::styled("q", key), Span::styled(" quit", dim)]);
        lines.push(Line::from(shortcuts));

        let paragraph = Paragraph::new(lines).block(crate::tui::sidecar_block());
        frame.render_widget(paragraph, frame.area());
    }
}

// ── Host state ──────────────────────────────────────────────

struct HostState {
    room_code: String,
    viewer_count: u32,
    status: String,
    rtt_ms: f64,
    packet_loss_pct: f64,
    has_measured_rtt: bool,
    target_bitrate_kbps: u32,
    target_fps: u32,
    source_width: u32,
    source_height: u32,
    output_width: u32,
    output_height: u32,
    max_quantizer: u32,
    bandwidth_kbps: u32,
    is_relay: bool,
    draw_mode: bool,
    overlay_running: bool,
    // Remote-control master switch state + UI.
    remote_control_enabled: bool,
    rc_controllers: u32,
    rc_prompt: bool,
    flash: Option<(String, std::time::Instant)>,
    warning: Option<String>,
    error: Option<String>,
}

impl HostState {
    fn new(room_code: String) -> Self {
        Self {
            room_code,
            viewer_count: 0,
            status: "waiting".to_string(),
            rtt_ms: 0.0,
            packet_loss_pct: 0.0,
            has_measured_rtt: false,
            target_bitrate_kbps: 0,
            target_fps: 0,
            source_width: 0,
            source_height: 0,
            output_width: 0,
            output_height: 0,
            max_quantizer: 0,
            bandwidth_kbps: 0,
            is_relay: false,
            draw_mode: false,
            overlay_running: false,
            remote_control_enabled: false,
            rc_controllers: 0,
            rc_prompt: false,
            flash: None,
            warning: None,
            error: None,
        }
    }

    fn apply_event(&mut self, event: PipelineEvent) {
        match event {
            PipelineEvent::ViewerJoined { viewer_count, .. } => {
                self.viewer_count = viewer_count;
            }
            PipelineEvent::ViewerLeft { viewer_count, .. } => {
                self.viewer_count = viewer_count;
            }
            PipelineEvent::ViewerCountSync { viewer_count } => {
                self.viewer_count = viewer_count;
            }
            PipelineEvent::RoomExpired => {
                self.status = "expired".to_string();
                self.error = Some("Room expired".to_string());
            }
            PipelineEvent::LatencyUpdate {
                rtt_ms,
                packet_loss_pct,
                status,
                has_measured_rtt,
            } => {
                self.rtt_ms = rtt_ms;
                self.packet_loss_pct = packet_loss_pct;
                self.status = status;
                self.has_measured_rtt = has_measured_rtt;
            }
            PipelineEvent::ResolutionUpdate {
                source_width,
                source_height,
                output_width,
                output_height,
                target_bitrate_kbps,
                max_quantizer,
                target_fps,
                bandwidth_kbps,
                is_relay,
            } => {
                self.source_width = source_width;
                self.source_height = source_height;
                self.output_width = output_width;
                self.output_height = output_height;
                self.target_bitrate_kbps = target_bitrate_kbps;
                self.max_quantizer = max_quantizer;
                self.target_fps = target_fps;
                self.bandwidth_kbps = bandwidth_kbps;
                self.is_relay = is_relay;
            }
            PipelineEvent::DataChannelState { .. } => {}
            PipelineEvent::ViewerAnnotation { .. } => {}
            PipelineEvent::RemoteControlRequested { .. } => {
                if !self.remote_control_enabled {
                    self.rc_prompt = true;
                }
            }
            PipelineEvent::RemoteControlActive {
                enabled,
                controllers,
            } => {
                self.remote_control_enabled = enabled;
                self.rc_controllers = controllers;
                if enabled {
                    self.rc_prompt = false;
                }
            }
            PipelineEvent::Warning { message } => {
                self.warning = Some(message);
            }
        }
    }

    fn render(&self, frame: &mut Frame) {
        let w = Style::default().fg(Color::White);
        let status_style = match self.status.as_str() {
            "connected" => Style::default().fg(Color::Green),
            "degraded" => Style::default().fg(Color::Yellow),
            "expired" => Style::default().fg(Color::Red),
            _ => Style::default().fg(Color::DarkGray),
        };

        let mut lines: Vec<Line> = Vec::new();

        // ── Session
        lines.push(Line::from(""));
        lines.push(row(
            "Room Code",
            &self.room_code,
            Style::default()
                .fg(crate::tui::ACCENT)
                .add_modifier(Modifier::BOLD),
        ));

        if let Some(ref warn) = self.warning {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("  {}", warn),
                Style::default().fg(Color::Yellow),
            )));
        }

        // ── Connection
        lines.push(Line::from(""));
        let status_value = if self.is_relay {
            format!("{} (relay)", self.status)
        } else {
            self.status.clone()
        };
        lines.push(row2(
            "Viewers",
            &self.viewer_count.to_string(),
            w,
            "Status",
            &status_value,
            status_style,
        ));

        if self.overlay_running {
            let (overlay_label, overlay_style) = if self.draw_mode {
                (
                    "drawing",
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                ("ready", Style::default().fg(Color::DarkGray))
            };
            lines.push(row("Overlay", overlay_label, overlay_style));
        }

        // Remote-control status
        let (rc_label, rc_style) = if self.remote_control_enabled {
            let label = if self.rc_controllers > 0 {
                format!("on ({} controlling)", self.rc_controllers)
            } else {
                "on".to_string()
            };
            (
                label,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            ("off".to_string(), Style::default().fg(Color::DarkGray))
        };
        lines.push(row("Remote control", &rc_label, rc_style));

        // Consent prompt when a viewer requests control while the switch is off.
        if self.rc_prompt {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  A viewer wants to control your screen.",
                Style::default().fg(Color::Yellow),
            )));
            let key = Style::default().fg(Color::White);
            let dim = Style::default().fg(Color::DarkGray);
            lines.push(Line::from(vec![
                Span::styled("  ", dim),
                Span::styled("a", key),
                Span::styled(" allow  ", dim),
                Span::styled("n", key),
                Span::styled(" deny", dim),
            ]));
        }

        // ── Stream
        if self.target_bitrate_kbps > 0 || self.target_fps > 0 {
            let resolution = if self.output_width > 0 && self.output_height > 0 {
                format!("{}x{}", self.output_width, self.output_height)
            } else {
                "auto".to_string()
            };

            lines.push(Line::from(""));
            lines.push(row2(
                "Resolution",
                &resolution,
                w,
                "FPS",
                &self.target_fps.to_string(),
                w,
            ));
            lines.push(row2(
                "Bitrate",
                &format!("{} kbps", self.target_bitrate_kbps),
                w,
                "QP",
                &self.max_quantizer.to_string(),
                w,
            ));

            // ── Network
            let rtt = if self.has_measured_rtt {
                format!("{:.0} ms", self.rtt_ms)
            } else {
                "--".to_string()
            };
            lines.push(Line::from(""));
            lines.push(row2(
                "RTT",
                &rtt,
                w,
                "Loss",
                &format!("{:.1}%", self.packet_loss_pct),
                w,
            ));
            lines.push(row(
                "Bandwidth",
                &format!("{} kbps", self.bandwidth_kbps),
                w,
            ));
        }

        lines.push(Line::from(""));

        if let Some(ref err) = self.error {
            lines.push(Line::from(Span::styled(
                format!("  {}", err),
                Style::default().fg(Color::Red),
            )));
        }

        // ── Shortcuts
        let dim = Style::default().fg(Color::DarkGray);
        let key = Style::default().fg(Color::White);
        let flash = self
            .flash
            .as_ref()
            .filter(|(_, instant)| instant.elapsed() < Duration::from_secs(2))
            .map(|(msg, _)| msg.as_str());
        let mut shortcuts = vec![Span::styled("  ", dim)];
        match flash {
            Some("Link copied!") => {
                shortcuts.extend([
                    Span::styled("Link copied!", Style::default().fg(Color::Green)),
                    Span::styled("  ", dim),
                ]);
            }
            Some(msg) => {
                shortcuts.extend([
                    Span::styled(msg.to_string(), Style::default().fg(Color::Yellow)),
                    Span::styled("  ", dim),
                ]);
            }
            None => {
                shortcuts.extend([Span::styled("c", key), Span::styled(" copy link  ", dim)]);
            }
        }
        if self.overlay_running {
            shortcuts.extend([Span::styled("d", key), Span::styled(" draw  ", dim)]);
        }
        shortcuts.extend([Span::styled("r", key), Span::styled(" remote  ", dim)]);
        shortcuts.extend([Span::styled("q", key), Span::styled(" quit", dim)]);
        lines.push(Line::from(shortcuts));

        let paragraph = Paragraph::new(lines)
            .block(crate::tui::sidecar_block())
            .wrap(ratatui::widgets::Wrap { trim: false });
        frame.render_widget(paragraph, frame.area());
    }
}

// ── Shared helpers ──────────────────────────────────────────

fn row(label: &str, value: &str, value_style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("  {:<width$}", label, width = COL),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(value.to_string(), value_style),
    ])
}

fn row2(l1: &str, v1: &str, s1: Style, l2: &str, v2: &str, s2: Style) -> Line<'static> {
    const RIGHT: usize = 14;
    let v1_pad = format!("{:<width$}", v1, width = RIGHT);
    Line::from(vec![
        Span::styled(
            format!("  {:<width$}", l1, width = COL),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(v1_pad, s1),
        Span::styled(
            format!("{:<width$}", l2, width = 8),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(v2.to_string(), s2),
    ])
}

pub fn viewer_link(room_code: &str) -> String {
    let base_url = std::env::var("SIDECAR_VIEWER_URL")
        .unwrap_or_else(|_| "https://view.sidecar.rs".to_string());
    format!("{}/?room={}", base_url, room_code)
}

fn copy_to_clipboard(text: &str) -> bool {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let Ok(mut child) = Command::new("wl-copy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let Some(mut stdin) = child.stdin.take() else {
        return false;
    };
    if stdin.write_all(text.as_bytes()).is_err() {
        return false;
    }
    drop(stdin);
    child.wait().map(|s| s.success()).unwrap_or(false)
}

// ── Main entry point ────────────────────────────────────────

pub fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut terminal = crate::tui::init_terminal()?;
    let result = run_inner(&mut terminal);
    let _ = crate::tui::restore_terminal();
    result
}

fn run_inner(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Create the runtime once so the D-Bus connection cached by ashpd's global
    // OnceLock survives across failed host-start attempts. Shutting down the
    // runtime between attempts would kill the D-Bus background tasks while
    // ashpd's cache still references the dead connection, causing the next
    // attempt to hang.
    let rt = tokio::runtime::Runtime::new()?;
    let mut landing = LandingState::new();

    loop {
        terminal.draw(|f| landing.render(f))?;

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            match landing.handle_key(key) {
                Action::StartHost => {
                    landing.error = None;
                    match start_host(&rt) {
                        Ok((mut state, ctx)) => {
                            return run_host_screen(terminal, &mut state, ctx, &rt);
                        }
                        Err(e) => landing.error = Some(e.to_string()),
                    }
                }
                Action::Join(code) => {
                    let _ = crate::tui::restore_terminal();
                    crate::run_join(&code);
                    return Ok(());
                }
                Action::Quit => return Ok(()),
                Action::None => {}
            }
        }
    }
}

struct HostContext {
    handle: pipeline::PipelineHandle,
    event_rx: mpsc::UnboundedReceiver<PipelineEvent>,
    overlay_handle: Option<crate::overlay::OverlayHandle>,
    overlay_event_rx: mpsc::UnboundedReceiver<crate::overlay::OverlayEvent>,
}

fn start_host(
    rt: &tokio::runtime::Runtime,
) -> Result<(HostState, HostContext), Box<dyn std::error::Error + Send + Sync>> {
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (handle, overlay_handle, overlay_event_rx) = rt.block_on(async {
        let handle = pipeline::start_pipeline(event_tx).await?;

        let (overlay_handle, overlay_event_rx) = {
            let annotation_tx = handle.annotation_tx();
            let resolution_override_tx = handle.resolution_override_tx.clone();
            match crate::overlay::spawn_overlay(annotation_tx, resolution_override_tx) {
                Ok((h, rx)) => {
                    tracing::info!("Overlay sidecar started");
                    (Some(h), rx)
                }
                Err(e) => {
                    tracing::warn!("Could not start overlay sidecar: {}", e);
                    let (_tx, rx) = mpsc::unbounded_channel();
                    (None, rx)
                }
            }
        };

        Ok::<_, Box<dyn std::error::Error + Send + Sync>>((
            handle,
            overlay_handle,
            overlay_event_rx,
        ))
    })?;

    let room_code = handle.room_code().to_string();
    let mut state = HostState::new(room_code);
    state.overlay_running = overlay_handle.is_some();

    let ctx = HostContext {
        handle,
        event_rx,
        overlay_handle,
        overlay_event_rx,
    };

    Ok((state, ctx))
}

fn run_host_screen(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut HostState,
    ctx: HostContext,
    rt: &tokio::runtime::Runtime,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let HostContext {
        handle,
        mut event_rx,
        overlay_handle,
        mut overlay_event_rx,
    } = ctx;

    rt.block_on(async {
        let mut key_reader = EventStream::new();

        loop {
            terminal.draw(|f| state.render(f))?;

            tokio::select! {
                event = event_rx.recv() => {
                    match event {
                        Some(evt) => {
                            match &evt {
                                PipelineEvent::ViewerAnnotation { data } => {
                                    if let Some(ref oh) = overlay_handle {
                                        oh.send_annotation(data);
                                    }
                                }
                                PipelineEvent::RemoteControlActive { enabled, .. } => {
                                    if let Some(ref oh) = overlay_handle {
                                        oh.send_remote_control_active(*enabled);
                                    }
                                }
                                _ => {}
                            }
                            state.apply_event(evt);
                        }
                        None => {
                            // Keep a more specific reason (e.g. "Room expired") if one
                            // was already reported before the session ended.
                            state
                                .error
                                .get_or_insert_with(|| "Session ended".to_string());
                            terminal.draw(|f| state.render(f))?;
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            break;
                        }
                    }
                }
                Some(overlay_evt) = overlay_event_rx.recv() => {
                    match overlay_evt {
                        crate::overlay::OverlayEvent::ModeChanged { draw } => {
                            state.draw_mode = draw;
                        }
                    }
                }
                Some(Ok(Event::Key(key))) = key_reader.next() => {
                    if key.kind == KeyEventKind::Press {
                        // Quit works regardless of any active prompt.
                        if matches!(key.code, KeyCode::Char('q') | KeyCode::Char('Q'))
                            || (key.code == KeyCode::Char('c')
                                && key.modifiers.contains(event::KeyModifiers::CONTROL))
                        {
                            break;
                        }
                        if state.rc_prompt {
                            // Consent prompt: allow (everyone in the session) / deny.
                            match key.code {
                                KeyCode::Char('a') => {
                                    state.rc_prompt = false;
                                    state.remote_control_enabled = true;
                                    handle.set_remote_control(true);
                                }
                                KeyCode::Char('n') | KeyCode::Esc => {
                                    state.rc_prompt = false;
                                }
                                _ => {}
                            }
                        } else {
                            match key.code {
                                KeyCode::Char('c') => {
                                    if copy_to_clipboard(&viewer_link(&state.room_code)) {
                                        state.flash = Some(("Link copied!".to_string(), std::time::Instant::now()));
                                    } else {
                                        state.flash = Some(("Copy failed (install wl-copy or xclip)".to_string(), std::time::Instant::now()));
                                    }
                                }
                                KeyCode::Char('d') => {
                                    if let Some(ref oh) = overlay_handle {
                                        state.draw_mode = !state.draw_mode;
                                        oh.send_set_mode(state.draw_mode);
                                    }
                                }
                                KeyCode::Char('r') => {
                                    let enabled = !state.remote_control_enabled;
                                    state.remote_control_enabled = enabled;
                                    handle.set_remote_control(enabled);
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }

        handle.stop().await;
        Ok(())
    })
}
