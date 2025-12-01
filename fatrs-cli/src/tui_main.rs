//! FAT Filesystem TUI Browser
//!
//! A terminal-based file manager for FAT filesystem images with hex viewer.

use std::io::{self, Stdout};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};

mod app;
use app::{App, InputMode, View};

// Type aliases for the specific FAT filesystem types we use
type TokioFile = embedded_io_adapters::tokio_1::FromTokio<tokio::fs::File>;
type FatApp = App<TokioFile, fatrs::DefaultTimeProvider, fatrs::LossyOemCpConverter>;

/// FAT Filesystem TUI Browser
#[derive(Parser, Debug)]
#[command(author, version, about = "TUI file browser for FAT filesystem images")]
struct Args {
    /// Path to FAT filesystem image
    #[arg(value_name = "IMAGE")]
    image: PathBuf,

    /// Open in read-only mode
    #[arg(short, long)]
    read_only: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Verify image exists
    if !args.image.exists() {
        anyhow::bail!("Image file does not exist: {}", args.image.display());
    }

    // Create tokio runtime
    let runtime = tokio::runtime::Runtime::new().context("Failed to create tokio runtime")?;

    // Open FAT filesystem
    let app = runtime.block_on(async {
        let file = tokio::fs::OpenOptions::new()
            .read(true)
            .write(!args.read_only)
            .open(&args.image)
            .await
            .with_context(|| format!("Failed to open image: {}", args.image.display()))?;

        let fs = fatrs::FileSystem::new(
            embedded_io_adapters::tokio_1::FromTokio::new(file),
            fatrs::FsOptions::new(),
        )
        .await
        .context("Failed to mount FAT filesystem")?;

        App::new(fs, runtime.handle().clone(), args.read_only)
    })?;

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Run the app
    let res = run_app(&mut terminal, app);

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        eprintln!("Error: {:?}", err);
    }

    // Keep runtime alive until app exits
    drop(runtime);

    Ok(())
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<Stdout>>, mut app: FatApp) -> Result<()> {
    // Initial directory load
    app.load_current_directory()?;

    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                // Clear any popup message on keypress
                app.message = None;

                match app.input_mode {
                    InputMode::Normal => match key.code {
                        KeyCode::Char('q') => return Ok(()),
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            return Ok(());
                        }
                        KeyCode::Up | KeyCode::Char('k') => app.previous(),
                        KeyCode::Down | KeyCode::Char('j') => app.next(),
                        KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                            app.enter_selected()?
                        }
                        KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => {
                            app.go_parent()?
                        }
                        KeyCode::Char('v') => app.view_file()?,
                        KeyCode::Char('x') => app.toggle_hex_view(),
                        KeyCode::Char('n') => app.start_create_file(),
                        KeyCode::Char('N') => app.start_create_dir(),
                        KeyCode::Char('d') => app.delete_selected()?,
                        KeyCode::Char('r') => app.start_rename(),
                        KeyCode::Char('R') => app.load_current_directory()?,
                        KeyCode::Char('?') => app.toggle_help(),
                        KeyCode::Esc => {
                            if app.view == View::Help {
                                app.view = View::Browser;
                            } else if app.view == View::FileContent || app.view == View::HexView {
                                app.view = View::Browser;
                            }
                        }
                        KeyCode::PageUp => app.scroll_up(20),
                        KeyCode::PageDown => app.scroll_down(20),
                        KeyCode::Home => app.scroll_to_top(),
                        KeyCode::End => app.scroll_to_bottom(),
                        _ => {}
                    },
                    InputMode::Input => match key.code {
                        KeyCode::Enter => app.confirm_input()?,
                        KeyCode::Esc => app.cancel_input(),
                        KeyCode::Char(c) => app.input_buffer.push(c),
                        KeyCode::Backspace => {
                            app.input_buffer.pop();
                        }
                        _ => {}
                    },
                }
            }
        }
    }
}

fn ui(f: &mut Frame, app: &mut FatApp) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(0),    // Main content
            Constraint::Length(3), // Footer/status
        ])
        .split(f.area());

    // Header
    let header = Paragraph::new(vec![Line::from(vec![
        Span::styled(
            " FAT Browser ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" | "),
        Span::styled(
            format!("/{}", app.current_path.join("/")),
            Style::default().fg(Color::Yellow),
        ),
        if app.read_only {
            Span::styled(" [READ-ONLY]", Style::default().fg(Color::Red))
        } else {
            Span::raw("")
        },
    ])])
    .block(Block::default().borders(Borders::ALL).title("fatrs"));
    f.render_widget(header, chunks[0]);

    // Main content based on view
    match app.view {
        View::Browser => render_browser(f, app, chunks[1]),
        View::FileContent => render_file_content(f, app, chunks[1]),
        View::HexView => render_hex_view(f, app, chunks[1]),
        View::Help => render_help(f, chunks[1]),
    }

    // Footer/status
    let status = match app.input_mode {
        InputMode::Normal => {
            let selected_info = if let Some(entry) = app.get_selected_entry() {
                format!(
                    "{} | {} | {}",
                    entry.name,
                    if entry.is_dir {
                        "DIR"
                    } else {
                        &format_size(entry.size)
                    },
                    entry.modified
                )
            } else {
                "Empty directory".to_string()
            };
            Paragraph::new(vec![
                Line::from(selected_info),
                Line::from(vec![
                    Span::styled(" q", Style::default().fg(Color::Cyan)),
                    Span::raw(":Quit "),
                    Span::styled("Enter", Style::default().fg(Color::Cyan)),
                    Span::raw(":Open "),
                    Span::styled("v", Style::default().fg(Color::Cyan)),
                    Span::raw(":View "),
                    Span::styled("x", Style::default().fg(Color::Cyan)),
                    Span::raw(":Hex "),
                    Span::styled("n", Style::default().fg(Color::Cyan)),
                    Span::raw(":New "),
                    Span::styled("N", Style::default().fg(Color::Cyan)),
                    Span::raw(":NewDir "),
                    Span::styled("d", Style::default().fg(Color::Cyan)),
                    Span::raw(":Del "),
                    Span::styled("r", Style::default().fg(Color::Cyan)),
                    Span::raw(":Rename "),
                    Span::styled(
                        "?",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(":Help", Style::default().fg(Color::Yellow)),
                ]),
            ])
        }
        InputMode::Input => Paragraph::new(vec![
            Line::from(vec![
                Span::raw(&app.input_prompt),
                Span::styled(&app.input_buffer, Style::default().fg(Color::Yellow)),
                Span::styled(
                    "_",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::SLOW_BLINK),
                ),
            ]),
            Line::from(vec![
                Span::styled(" Enter", Style::default().fg(Color::Cyan)),
                Span::raw(":Confirm "),
                Span::styled("Esc", Style::default().fg(Color::Cyan)),
                Span::raw(":Cancel"),
            ]),
        ]),
    };
    let status = status.block(Block::default().borders(Borders::ALL).title("Status"));
    f.render_widget(status, chunks[2]);

    // Render popup for messages
    if let Some(ref msg) = app.message {
        render_popup(f, msg, f.area());
    }
}

fn render_browser(f: &mut Frame, app: &mut FatApp, area: Rect) {
    let items: Vec<ListItem> = app
        .entries
        .iter()
        .map(|entry| {
            let icon = if entry.is_dir { " " } else { " " };
            let style = if entry.is_dir {
                Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            let size_str = if entry.is_dir {
                "<DIR>".to_string()
            } else {
                format_size(entry.size)
            };
            ListItem::new(Line::from(vec![
                Span::styled(icon, style),
                Span::styled(&entry.name, style),
                Span::raw("  "),
                Span::styled(size_str, Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Files ({}) ", app.entries.len())),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");

    f.render_stateful_widget(list, area, &mut app.list_state);
}

fn render_file_content(f: &mut Frame, app: &FatApp, area: Rect) {
    let content = app.file_content.as_deref().unwrap_or("No content");
    let lines: Vec<Line> = content
        .lines()
        .skip(app.scroll_offset)
        .take(area.height as usize - 2)
        .map(|line| Line::from(line.to_string()))
        .collect();

    let paragraph = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(format!(
            " {} (text) ",
            app.viewing_file.as_deref().unwrap_or("")
        )))
        .wrap(Wrap { trim: false });

    f.render_widget(paragraph, area);
}

fn render_hex_view(f: &mut Frame, app: &FatApp, area: Rect) {
    let bytes = app.file_bytes.as_deref().unwrap_or(&[]);
    let bytes_per_line = 16;
    let visible_lines = (area.height as usize).saturating_sub(2);
    let start_line = app.scroll_offset;

    let mut lines: Vec<Line> = Vec::new();

    for line_idx in start_line..(start_line + visible_lines) {
        let offset = line_idx * bytes_per_line;
        if offset >= bytes.len() {
            break;
        }

        let end = (offset + bytes_per_line).min(bytes.len());
        let line_bytes = &bytes[offset..end];

        // Offset
        let mut spans = vec![Span::styled(
            format!("{:08X}  ", offset),
            Style::default().fg(Color::DarkGray),
        )];

        // Hex bytes
        for (i, byte) in line_bytes.iter().enumerate() {
            if i == 8 {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(
                format!("{:02X} ", byte),
                Style::default().fg(Color::Cyan),
            ));
        }

        // Padding for incomplete lines
        for i in line_bytes.len()..bytes_per_line {
            if i == 8 {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::raw("   "));
        }

        spans.push(Span::raw(" |"));

        // ASCII representation
        for byte in line_bytes {
            let c = if *byte >= 0x20 && *byte < 0x7F {
                *byte as char
            } else {
                '.'
            };
            spans.push(Span::styled(
                c.to_string(),
                Style::default().fg(Color::Yellow),
            ));
        }

        spans.push(Span::raw("|"));
        lines.push(Line::from(spans));
    }

    let paragraph =
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(format!(
            " {} (hex - {} bytes) ",
            app.viewing_file.as_deref().unwrap_or(""),
            bytes.len()
        )));

    f.render_widget(paragraph, area);
}

fn render_help(f: &mut Frame, area: Rect) {
    let help_text = vec![
        Line::from(Span::styled(
            "Navigation",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("  j/k or Up/Down    Move cursor"),
        Line::from("  Enter/l/Right     Open file/directory"),
        Line::from("  Backspace/h/Left  Go to parent directory"),
        Line::from("  PageUp/PageDown   Scroll content"),
        Line::from("  Home/End          Scroll to top/bottom"),
        Line::from(""),
        Line::from(Span::styled(
            "Viewing",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("  v                 View file as text"),
        Line::from("  x                 View file as hex"),
        Line::from("  Esc               Close viewer"),
        Line::from(""),
        Line::from(Span::styled(
            "File Operations",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("  n                 Create new file"),
        Line::from("  N                 Create new directory"),
        Line::from("  d                 Delete selected"),
        Line::from("  r                 Rename selected"),
        Line::from("  R                 Refresh directory"),
        Line::from(""),
        Line::from(Span::styled(
            "General",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("  ?                 Toggle this help"),
        Line::from("  q / Ctrl+C        Quit"),
    ];

    let paragraph =
        Paragraph::new(help_text).block(Block::default().borders(Borders::ALL).title(" Help "));

    f.render_widget(paragraph, area);
}

fn render_popup(f: &mut Frame, message: &str, area: Rect) {
    let popup_width = 50.min(area.width - 4);
    let popup_height = 5;
    let popup_area = Rect {
        x: (area.width - popup_width) / 2,
        y: (area.height - popup_height) / 2,
        width: popup_width,
        height: popup_height,
    };

    f.render_widget(Clear, popup_area);
    let popup = Paragraph::new(message)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Message ")
                .style(Style::default().bg(Color::DarkGray)),
        )
        .wrap(Wrap { trim: true });
    f.render_widget(popup, popup_area);
}

fn format_size(size: u64) -> String {
    if size < 1024 {
        format!("{} B", size)
    } else if size < 1024 * 1024 {
        format!("{:.1} KB", size as f64 / 1024.0)
    } else if size < 1024 * 1024 * 1024 {
        format!("{:.1} MB", size as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", size as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}
