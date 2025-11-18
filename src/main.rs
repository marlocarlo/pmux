use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use portable_pty::{CommandBuilder, MasterPty, PtySize, PtySystemSelection};
use ratatui::{prelude::*, widgets::*};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use crossterm::terminal::{enable_raw_mode, disable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute};
use crossterm::cursor::{EnableBlinking, DisableBlinking};
use ratatui::style::{Style, Modifier};
use chrono::Local;
use std::env;
use crossterm::style::Print;

struct Pane {
    master: Box<dyn MasterPty>,
    child: Box<dyn portable_pty::Child>,
    term: Arc<Mutex<vt100::Parser>>,
    last_rows: u16,
    last_cols: u16,
}

enum LayoutKind { Horizontal, Vertical }

struct Window {
    panes: Vec<Pane>,
    active_pane: usize,
    layout: LayoutKind,
}

enum Mode {
    Passthrough,
    Prefix { armed_at: Instant },
    CommandPrompt { input: String },
}

struct AppState {
    windows: Vec<Window>,
    active_idx: usize,
    mode: Mode,
    escape_time_ms: u64,
    prefix_key: (KeyCode, KeyModifiers),
}

fn main() -> io::Result<()> {
    if env::var("RMUX_ACTIVE").ok().as_deref() == Some("1") {
        eprintln!("rmux: nested sessions are not allowed");
        return Ok(());
    }
    env::set_var("RMUX_ACTIVE", "1");
    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen, EnableBlinking)?;
    apply_cursor_style(&mut stdout)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run(&mut terminal);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), DisableBlinking, LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    let pty_system = PtySystemSelection::default()
        .get()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("pty system error: {e}")))?;

    let mut app = AppState {
        windows: Vec::new(),
        active_idx: 0,
        mode: Mode::Passthrough,
        escape_time_ms: 500,
        prefix_key: (KeyCode::Char('b'), KeyModifiers::CONTROL),
    };

    create_window(&*pty_system, &mut app)?;

    let mut last_resize = Instant::now();
    let mut quit = false;
    loop {
        terminal.draw(|f| {
            let area = f.size();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(1), Constraint::Length(1)].as_ref())
                .split(area);

            let win = &mut app.windows[app.active_idx];
            let pane_count = win.panes.len().max(1);
            let pane_chunks = match win.layout {
                LayoutKind::Horizontal => Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints(vec![Constraint::Percentage((100 / pane_count) as u16); pane_count])
                    .split(chunks[0]),
                LayoutKind::Vertical => Layout::default()
                    .direction(Direction::Vertical)
                    .constraints(vec![Constraint::Percentage((100 / pane_count) as u16); pane_count])
                    .split(chunks[0]),
            };
            for (i, pane) in win.panes.iter_mut().enumerate() {
                let outer = pane_chunks[i];
                let title = if i == win.active_pane { format!("* pane {}", i + 1) } else { format!("  pane {}", i + 1) };
                let pane_block = Block::default().borders(Borders::ALL).title(title);
                let inner = pane_block.inner(outer);

                let target_rows = inner.height.max(1);
                let target_cols = inner.width.max(1);
                if pane.last_rows != target_rows || pane.last_cols != target_cols {
                    let _ = pane.master.resize(PtySize {
                        rows: target_rows,
                        cols: target_cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    });
                    let mut parser = pane.term.lock().unwrap();
                    parser.screen_mut().set_size(target_rows, target_cols);
                    pane.last_rows = target_rows;
                    pane.last_cols = target_cols;
                }

                let parser = pane.term.lock().unwrap();
                let screen = parser.screen();
                let mut lines: Vec<Line> = Vec::with_capacity(target_rows as usize);
                for r in 0..target_rows {
                    let mut spans: Vec<Span> = Vec::with_capacity(target_cols as usize);
                    for c in 0..target_cols {
                        if let Some(cell) = screen.cell(r, c) {
                            let mut fg = vt_to_color(cell.fgcolor());
                            let mut bg = vt_to_color(cell.bgcolor());
                            if cell.inverse() { std::mem::swap(&mut fg, &mut bg); }
                            let mut style = Style::default().fg(fg).bg(bg);
                            if cell.bold() { style = style.add_modifier(Modifier::BOLD); }
                            if cell.italic() { style = style.add_modifier(Modifier::ITALIC); }
                            if cell.underline() { style = style.add_modifier(Modifier::UNDERLINED); }
                            let text = cell.contents().to_string();
                            spans.push(Span::styled(text, style));
                        } else {
                            spans.push(Span::raw(" "));
                        }
                    }
                    lines.push(Line::from(spans));
                }

                f.render_widget(pane_block, outer);
                f.render_widget(Clear, inner);
                let para = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
                f.render_widget(para, inner);
                if i == win.active_pane {
                    let (cr, cc) = screen.cursor_position();
                    let cr = cr.min(target_rows.saturating_sub(1));
                    let cc = cc.min(target_cols.saturating_sub(1));
                    let cx = inner.x + cc;
                    let cy = inner.y + cr;
                    f.set_cursor(cx, cy);
                }
            }

            let mode_str = match app.mode { Mode::Passthrough => "", Mode::Prefix { .. } => "PREFIX", Mode::CommandPrompt { .. } => ":" };
            let time_str = Local::now().format("%H:%M").to_string();
            let mut windows_list = String::new();
            for (i, _) in app.windows.iter().enumerate() {
                if i == app.active_idx { windows_list.push_str(&format!(" #[{}]", i+1)); } else { windows_list.push_str(&format!(" {}", i+1)); }
            }
            let status_text = format!(" {} | {} | {} ", mode_str, windows_list.trim(), time_str);
            let status_bar = Paragraph::new(Line::from(status_text)).style(Style::default().bg(Color::Green).fg(Color::Black));
            f.render_widget(Clear, chunks[1]);
            f.render_widget(status_bar, chunks[1]);

            if let Mode::CommandPrompt { input } = &app.mode {
                let overlay = Paragraph::new(format!(":{}", input)).block(Block::default().borders(Borders::ALL).title("command"));
                let oa = centered_rect(80, 3, area);
                f.render_widget(Clear, oa);
                f.render_widget(overlay, oa);
            }
        })?;

        if event::poll(Duration::from_millis(20))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if handle_key(&mut app, key)? {
                        quit = true;
                    }
                }
                Event::Resize(cols, rows) => {
                    if last_resize.elapsed() > Duration::from_millis(50) {
                        let win = &mut app.windows[app.active_idx];
                        let _ = win.panes[win.active_pane].master.resize(PtySize {
                            rows: rows as u16,
                            cols: cols as u16,
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                        if let Some(pane) = win.panes.get_mut(win.active_pane) {
                            let mut parser = pane.term.lock().unwrap();
                            parser.screen_mut().set_size(rows, cols);
                        }
                        last_resize = Instant::now();
                    }
                }
                _ => {}
            }
        }

        if reap_children(&mut app)? {
            quit = true;
        }

        if quit { break; }
    }
    // teardown: kill all pane children
    for win in app.windows.iter_mut() {
        for pane in win.panes.iter_mut() {
            let _ = pane.child.kill();
        }
    }
    Ok(())
}

fn create_window(pty_system: &dyn portable_pty::PtySystem, app: &mut AppState) -> io::Result<()> {
    let size = PtySize { rows: 30, cols: 120, pixel_width: 0, pixel_height: 0 };
    let mut pair = pty_system
        .openpty(size)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("openpty error: {e}")))?;

    let shell_cmd = detect_shell();
    let child = pair
        .slave
        .spawn_command(shell_cmd)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("spawn shell error: {e}")))?;

    let term: Arc<Mutex<vt100::Parser>> = Arc::new(Mutex::new(vt100::Parser::new(size.rows, size.cols, 0)));
    let term_reader = term.clone();
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("clone reader error: {e}")))?;

    thread::spawn(move || {
        let mut local = [0u8; 8192];
        loop {
            match reader.read(&mut local) {
                Ok(n) if n > 0 => {
                    let mut parser = term_reader.lock().unwrap();
                    parser.process(&local[..n]);
                }
                Ok(_) => thread::sleep(Duration::from_millis(5)),
                Err(_) => break,
            }
        }
    });

    let pane = Pane { master: pair.master, child, term, last_rows: size.rows, last_cols: size.cols };
    app.windows.push(Window { panes: vec![pane], active_pane: 0, layout: LayoutKind::Horizontal });
    app.active_idx = app.windows.len() - 1;
    Ok(())
}

fn handle_key(app: &mut AppState, key: KeyEvent) -> io::Result<bool> {
    if matches!(key.code, KeyCode::Char('q')) && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Ok(true);
    }

    match app.mode {
        Mode::Passthrough => {
            let is_ctrl_b = (key.code, key.modifiers) == app.prefix_key
                || matches!(key.code, KeyCode::Char(c) if c == '\u{0002}');
            if is_ctrl_b {
                app.mode = Mode::Prefix { armed_at: Instant::now() };
                return Ok(false);
            }
            forward_key_to_active(app, key)?;
            Ok(false)
        }
        Mode::Prefix { armed_at } => {
            let elapsed = armed_at.elapsed().as_millis() as u64;
            let handled = match key.code {
                KeyCode::Char(d) if d.is_ascii_digit() => {
                    let idx = d.to_digit(10).unwrap() as usize;
                    if idx > 0 && idx <= app.windows.len() { app.active_idx = idx - 1; }
                    true
                }
                KeyCode::Char('c') => {
                    let pty_system = PtySystemSelection::default()
                        .get()
                        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("pty system error: {e}")))?;
                    create_window(&*pty_system, app)?;
                    true
                }
                KeyCode::Char('n') => {
                    if !app.windows.is_empty() {
                        app.active_idx = (app.active_idx + 1) % app.windows.len();
                    }
                    true
                }
                KeyCode::Char('p') => {
                    if !app.windows.is_empty() {
                        app.active_idx = (app.active_idx + app.windows.len() - 1) % app.windows.len();
                    }
                    true
                }
                KeyCode::Char('%') => {
                    split_active(app, LayoutKind::Vertical)?;
                    true
                }
                KeyCode::Char('"') => {
                    split_active(app, LayoutKind::Horizontal)?;
                    true
                }
                KeyCode::Char('x') => {
                    kill_active_pane(app)?;
                    true
                }
                KeyCode::Char(':') => {
                    app.mode = Mode::CommandPrompt { input: String::new() };
                    true
                }
                _ => false,
            };

            app.mode = Mode::Passthrough;
            if !handled && elapsed < app.escape_time_ms {
                // Unrecognized after prefix: do not send '^B'; swallow and return
                return Ok(false);
            }
            Ok(false)
        }
        Mode::CommandPrompt { .. } => {
            match key.code {
                KeyCode::Esc => { app.mode = Mode::Passthrough; }
                KeyCode::Enter => { execute_command_prompt(app)?; }
                KeyCode::Backspace => {
                    if let Mode::CommandPrompt { input } = &mut app.mode { let _ = input.pop(); }
                }
                KeyCode::Char(c) => {
                    if let Mode::CommandPrompt { input } = &mut app.mode { input.push(c); }
                }
                _ => {}
            }
            Ok(false)
        }
    }
}

fn forward_key_to_active(app: &mut AppState, key: KeyEvent) -> io::Result<()> {
    let win = &mut app.windows[app.active_idx];
    let active = &mut win.panes[win.active_pane];
    match key.code {
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            let _ = write!(active.master, "{}", c);
        }
        KeyCode::Enter => { let _ = write!(active.master, "\r"); }
        KeyCode::Tab => { let _ = write!(active.master, "\t"); }
        KeyCode::Backspace => { let _ = write!(active.master, "\x08"); }
        KeyCode::Esc => { let _ = write!(active.master, "\x1b"); }
        KeyCode::Left => { let _ = write!(active.master, "\x1b[D"); }
        KeyCode::Right => { let _ = write!(active.master, "\x1b[C"); }
        KeyCode::Up => { let _ = write!(active.master, "\x1b[A"); }
        KeyCode::Down => { let _ = write!(active.master, "\x1b[B"); }
        _ => {}
    }
    Ok(())
}

fn split_active(app: &mut AppState, kind: LayoutKind) -> io::Result<()> {
    let pty_system = PtySystemSelection::default()
        .get()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("pty system error: {e}")))?;
    let size = PtySize { rows: 30, cols: 120, pixel_width: 0, pixel_height: 0 };
    let mut pair = pty_system
        .openpty(size)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("openpty error: {e}")))?;
    let shell_cmd = detect_shell();
    let child = pair
        .slave
        .spawn_command(shell_cmd)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("spawn shell error: {e}")))?;
    let term: Arc<Mutex<vt100::Parser>> = Arc::new(Mutex::new(vt100::Parser::new(size.rows, size.cols, 0)));
    let term_reader = term.clone();
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("clone reader error: {e}")))?;
    thread::spawn(move || {
        let mut local = [0u8; 8192];
        loop {
            match reader.read(&mut local) {
                Ok(n) if n > 0 => {
                    let mut parser = term_reader.lock().unwrap();
                    parser.process(&local[..n]);
                }
                Ok(_) => thread::sleep(Duration::from_millis(5)),
                Err(_) => break,
            }
        }
    });
    let pane = Pane { master: pair.master, child, term, last_rows: size.rows, last_cols: size.cols };
    let win = &mut app.windows[app.active_idx];
    win.panes.push(pane);
    win.active_pane = win.panes.len() - 1;
    win.layout = kind;
    Ok(())
}

fn kill_active_pane(app: &mut AppState) -> io::Result<()> {
    let win = &mut app.windows[app.active_idx];
    if win.panes.len() <= 1 { return Ok(()); }
    let idx = win.active_pane;
    let mut pane = win.panes.remove(idx);
    let _ = pane.child.kill();
    if win.active_pane >= win.panes.len() { win.active_pane = win.panes.len().saturating_sub(1); }
    Ok(())
}

fn detect_shell() -> CommandBuilder {
    let pwsh = which::which("pwsh").ok().map(|p| p.to_string_lossy().into_owned());
    let cmd = which::which("cmd").ok().map(|p| p.to_string_lossy().into_owned());
    match pwsh.or(cmd) {
        Some(path) => CommandBuilder::new(path),
        None => CommandBuilder::new("pwsh.exe"),
    }
}

fn execute_command_prompt(app: &mut AppState) -> io::Result<()> {
    let cmdline = match &app.mode { Mode::CommandPrompt { input } => input.clone(), _ => String::new() };
    app.mode = Mode::Passthrough;
    let parts: Vec<&str> = cmdline.split_whitespace().collect();
    if parts.is_empty() { return Ok(()); }
    match parts[0] {
        "new-window" => {
            let pty_system = PtySystemSelection::default().get().map_err(|e| io::Error::new(io::ErrorKind::Other, format!("pty system error: {e}")))?;
            create_window(&*pty_system, app)?;
        }
        "split-window" => {
            let kind = if parts.iter().any(|p| *p == "-h") { LayoutKind::Horizontal } else { LayoutKind::Vertical };
            split_active(app, kind)?;
        }
        "kill-pane" => { kill_active_pane(app)?; }
        "next-window" => { app.active_idx = (app.active_idx + 1) % app.windows.len(); }
        "previous-window" => { app.active_idx = (app.active_idx + app.windows.len() - 1) % app.windows.len(); }
        "select-window" => {
            if let Some(tidx) = parts.iter().position(|p| *p == "-t").and_then(|i| parts.get(i+1)) { if let Ok(n) = tidx.parse::<usize>() { if n>0 && n<=app.windows.len() { app.active_idx = n-1; } } }
        }
        _ => {}
    }
    Ok(())
}

fn centered_rect(percent_x: u16, height: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(50),
            Constraint::Length(height),
            Constraint::Percentage(50),
        ])
        .split(r);
    let middle = popup_layout[1];
    let width = (middle.width * percent_x) / 100;
    let x = middle.x + (middle.width - width) / 2;
    Rect { x, y: middle.y, width, height }
}

fn reap_children(app: &mut AppState) -> io::Result<bool> {
    let mut i = 0;
    while i < app.windows.len() {
        let win = &mut app.windows[i];
        let mut j = 0;
        while j < win.panes.len() {
            let pane = &mut win.panes[j];
            match pane.child.try_wait() {
                Ok(Some(_)) => {
                    win.panes.remove(j);
                    if win.active_pane >= win.panes.len() {
                        win.active_pane = win.panes.len().saturating_sub(1);
                    }
                    continue;
                }
                Ok(None) => {}
                Err(_) => {}
            }
            j += 1;
        }
        if win.panes.is_empty() {
            app.windows.remove(i);
            if app.active_idx >= app.windows.len() {
                app.active_idx = app.windows.len().saturating_sub(1);
            }
            continue;
        }
        i += 1;
    }
    Ok(app.windows.is_empty())
}

fn vt_to_color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => match i {
            0 => Color::Black,
            1 => Color::Red,
            2 => Color::Green,
            3 => Color::Yellow,
            4 => Color::Blue,
            5 => Color::Magenta,
            6 => Color::Cyan,
            7 => Color::Gray,
            8 => Color::DarkGray,
            9 => Color::LightRed,
            10 => Color::LightGreen,
            11 => Color::LightYellow,
            12 => Color::LightBlue,
            13 => Color::LightMagenta,
            14 => Color::LightCyan,
            15 => Color::White,
            _ => Color::Reset,
        },
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

fn apply_cursor_style<W: Write>(out: &mut W) -> io::Result<()> {
    let style = env::var("RMUX_CURSOR_STYLE").unwrap_or_else(|_| "bar".to_string());
    let blink = env::var("RMUX_CURSOR_BLINK").unwrap_or_else(|_| "1".to_string()) != "0";
    let code = match style.as_str() {
        "block" => if blink { 1 } else { 2 },
        "underline" => if blink { 3 } else { 4 },
        "bar" | "beam" => if blink { 5 } else { 6 },
        _ => if blink { 5 } else { 6 },
    };
    execute!(out, Print(format!("\x1b[{} q", code)))?;
    Ok(())
}