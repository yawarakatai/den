use std::{
    io::{self, IsTerminal},
    path::Path,
    time::Instant,
};

use anyhow::{Context, Result, bail};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{Axis, Block, BorderType, Chart, Clear, Dataset, Gauge, GraphType, Paragraph, Wrap},
};

use crate::{
    eta,
    model::{BatteryReading, Sample},
    monitor::{LIVE_INTERVAL, Monitor},
    predict::{Estimate, format_duration},
    usage::{HOUR, HOURS},
};

const RANGES: [i64; 7] = [60, 300, 900, 3_600, 21_600, 86_400, 604_800];
const RANGE_NAMES: [&str; 7] = ["60s", "5m", "15m", "1h", "6h", "24h", "7d"];
const TEXT: Color = Color::Rgb(220, 228, 240);
const SECONDARY: Color = Color::Rgb(186, 198, 216);
const MUTED: Color = Color::Rgb(135, 147, 170);
const BORDER: Color = Color::Rgb(73, 86, 113);
const BLUE: Color = Color::Rgb(143, 181, 232);
const GREEN: Color = Color::Rgb(139, 197, 146);
const ORANGE: Color = Color::Rgb(209, 139, 71);
const YELLOW: Color = Color::Rgb(238, 199, 113);
const RED: Color = Color::Rgb(240, 133, 148);

#[derive(Clone, Copy, Default, PartialEq)]
enum Graph {
    #[default]
    Power,
    Charge,
}

#[derive(Default)]
struct View {
    range: usize,
    focus: Graph,
    expanded: bool,
    help: bool,
    details: Option<bool>,
}

struct Screen;
impl Drop for Screen {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

pub fn run(path: &Path, sysfs: &Path, history_only: bool) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("The monitor requires an interactive terminal");
    }
    let mut monitor = Monitor::new(path, sysfs, !history_only)?;
    let mut view = View::default();
    monitor.tick(RANGES[view.range])?;
    if history_only && let Some(error) = &monitor.history_error {
        bail!("{error}");
    }
    enable_raw_mode().context("Cannot enable terminal raw mode")?;
    let _screen = Screen;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut next_tick = Instant::now() + LIVE_INTERVAL;
    let mut dirty = true;
    loop {
        if dirty {
            terminal.draw(|frame| draw(frame, &monitor, &view, path))?;
            dirty = false;
        }
        if event::poll(next_tick.saturating_duration_since(Instant::now()))? {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if key.code == KeyCode::Char('q')
                        || (key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL))
                        || (key.code == KeyCode::Esc && !view.help && view.details != Some(true))
                    {
                        break;
                    }
                    let mut reload = false;
                    match key.code {
                        KeyCode::Up => view.focus = Graph::Power,
                        KeyCode::Down => view.focus = Graph::Charge,
                        KeyCode::Tab => {
                            view.focus = if view.focus == Graph::Power {
                                Graph::Charge
                            } else {
                                Graph::Power
                            }
                        }
                        KeyCode::Left | KeyCode::Char('-') => {
                            view.range = view.range.saturating_sub(1);
                            reload = true;
                        }
                        KeyCode::Right | KeyCode::Char('+') => {
                            view.range = (view.range + 1).min(RANGES.len() - 1);
                            reload = true;
                        }
                        KeyCode::Char('b') => {
                            monitor.next_battery();
                            reload = true;
                        }
                        KeyCode::Char('r') => reload = true,
                        KeyCode::Char('f') => view.expanded = !view.expanded,
                        KeyCode::Char('?') => view.help = !view.help,
                        KeyCode::Char('d') => {
                            let size = terminal.size()?;
                            let automatic =
                                sidebar_available(size.width, size.height, view.expanded);
                            view.details = Some(!view.details.unwrap_or(automatic));
                            view.help = false;
                        }
                        KeyCode::Esc => {
                            if view.help {
                                view.help = false;
                            } else {
                                view.details = Some(false);
                            }
                        }
                        _ => (),
                    }
                    if reload {
                        monitor.refresh_history(RANGES[view.range]);
                    }
                    dirty = true;
                }
                Event::Resize(_, _) => dirty = true,
                _ => (),
            }
        }
        if Instant::now() >= next_tick {
            monitor.tick(RANGES[view.range])?;
            next_tick = Instant::now() + LIVE_INTERVAL;
            dirty = true;
        }
    }
    Ok(())
}

fn panel(title: impl Into<Line<'static>>, color: Color) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(color))
        .title(title.into().style(Style::default().fg(TEXT)))
}

fn draw(frame: &mut Frame<'_>, monitor: &Monitor, view: &View, path: &Path) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::default().fg(TEXT)), area);
    if area.width < 48 || area.height < 18 {
        frame.render_widget(
            Paragraph::new("Terminal too small (minimum 48 x 18)")
                .style(Style::default().fg(YELLOW)),
            area,
        );
        return;
    }
    let compact = area.width < 72;
    let rows = Layout::vertical([
        Constraint::Length(if compact { 8 } else { 4 }),
        Constraint::Min(5),
        Constraint::Length(1),
    ])
    .spacing(1)
    .split(area);
    let cards = metric_areas(rows[0], compact);
    draw_metrics(frame, monitor, &cards);
    let sidebar =
        sidebar_available(area.width, area.height, view.expanded) && view.details.unwrap_or(true);
    if view.expanded || rows[1].height < 12 {
        draw_chart(frame, monitor, view, view.focus, rows[1], true);
    } else {
        let constraints = if sidebar {
            // Keep metadata compact instead of stretching it through the full
            // screen. Both columns share this boundary so all four frames align.
            let lower = ((rows[1].height - 1) * 2 / 5).clamp(9, 12);
            [Constraint::Min(6), Constraint::Length(lower)]
        } else {
            [Constraint::Percentage(60), Constraint::Percentage(40)]
        };
        let plots = Layout::vertical(constraints).spacing(1).split(rows[1]);
        for (index, graph) in [Graph::Power, Graph::Charge].into_iter().enumerate() {
            let mut plot = plots[index];
            if sidebar {
                let right = Rect::new(cards[3].x, plot.y, cards[3].width, plot.height);
                if graph == Graph::Power {
                    draw_usage(frame, monitor, right);
                } else {
                    draw_details(frame, monitor, right, false);
                }
                plot.width = cards[2].right() - plot.x;
            }
            draw_chart(frame, monitor, view, graph, plot, index == 0);
        }
    }
    draw_footer(frame, monitor, rows[2]);
    if view.help {
        draw_help(frame, monitor, view, path);
    } else if view.details == Some(true) && !sidebar {
        draw_details_popup(frame, monitor);
    }
}

fn sidebar_available(width: u16, height: u16, expanded: bool) -> bool {
    width >= 120 && height >= 24 && !expanded
}

fn draw_usage(frame: &mut Frame<'_>, monitor: &Monitor, area: Rect) {
    let block = panel(" Battery use · Wh ", BORDER);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }
    let subtitle = "24h · hourly UTC";
    let Some(usage) = &monitor.usage else {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(subtitle, Style::default().fg(SECONDARY)),
                Line::from(""),
                Line::styled(
                    if monitor.usage_error.is_some() {
                        "History unavailable"
                    } else {
                        "No saved history"
                    },
                    Style::default().fg(SECONDARY),
                ),
            ]),
            inner,
        );
        return;
    };
    let observed = usage.total().map_or_else(
        || "Observed N/A".into(),
        |energy| format!("Observed {} Wh", usage_number(energy)),
    );
    if inner.height < 9 || inner.width < HOURS as u16 {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(subtitle),
                Line::from(observed),
                Line::styled("Enlarge for hourly bars", Style::default().fg(SECONDARY)),
            ]),
            inner,
        );
        return;
    }
    let highest = usage
        .hours
        .iter()
        .map(|hour| hour.energy_wh)
        .fold(0.0, f64::max);
    let scale = if highest > 0.0 {
        let step = (10.0_f64.powf(highest.log10().floor()) / 2.0).max(f64::MIN_POSITIVE);
        let rounded = (highest / step).ceil() * step;
        if rounded.is_finite() {
            rounded
        } else {
            highest
        }
    } else {
        1.0
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(subtitle, Style::default().fg(SECONDARY)),
            Line::from(observed),
            Line::styled(
                format!("0–{} Wh", usage_number(scale)),
                Style::default().fg(SECONDARY),
            ),
        ]),
        Rect::new(inner.x, inner.y, inner.width, 3),
    );
    let plot = Rect::new(inner.x, inner.y + 3, inner.width, inner.height - 6);
    let blocks = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
    for (index, hour) in usage.hours.iter().enumerate() {
        let left = plot.x + index as u16 * plot.width / HOURS as u16;
        let right = plot.x + (index as u16 + 1) * plot.width / HOURS as u16;
        let width = (right - left).saturating_sub(1).max(1);
        let units = ((hour.energy_wh / scale) * f64::from(plot.height) * 8.0)
            .round()
            .max(if hour.energy_wh > 0.0 { 1.0 } else { 0.0 }) as u16;
        for row in 0..plot.height {
            let level = units.saturating_sub(row * 8).min(8);
            if level > 0 {
                for x in left..left + width {
                    frame.buffer_mut()[(x, plot.bottom() - 1 - row)]
                        .set_symbol(blocks[level as usize])
                        .set_fg(TEXT);
                }
            }
        }
        let current = hour.start <= monitor.plot_end() && monitor.plot_end() < hour.start + HOUR;
        let marker = if hour.energy().is_none() {
            "·"
        } else if current {
            "*"
        } else if hour.covered_seconds < HOUR {
            "~"
        } else if hour.energy_wh == 0.0 {
            "0"
        } else {
            " "
        };
        frame.buffer_mut()[(left, plot.bottom())]
            .set_symbol(marker)
            .set_fg(if marker == "·" { MUTED } else { SECONDARY });
        if index % 6 == 0 || index == HOURS - 1 {
            frame.render_widget(
                Paragraph::new(format!("{:02}", hour.start.div_euclid(HOUR).rem_euclid(24)))
                    .style(Style::default().fg(SECONDARY)),
                Rect::new(left, plot.bottom() + 1, 2.min(inner.right() - left), 1),
            );
        }
    }
    frame.render_widget(
        Paragraph::new("* now  ~ gaps  · no data").style(Style::default().fg(SECONDARY)),
        Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
    );
}

fn usage_number(value: f64) -> String {
    if value > 0.0 && value < 0.0001 {
        format!("{value:.1e}")
    } else if value > 0.0 && value < 1.0 {
        // A newly recorded fraction of a Wh should not look like zero use.
        let decimals = (1.0 - value.log10().floor()) as usize;
        format!("{value:.decimals$}")
    } else {
        format!("{value:.1}")
    }
}

fn accent(monitor: &Monitor) -> Color {
    if monitor.source_stale() {
        YELLOW
    } else {
        monitor
            .reading()
            .map_or(MUTED, |reading| status_color(&reading.status))
    }
}

fn range_selector(view: &View, color: Color, available: usize) -> Line<'static> {
    let left = Span::styled(
        " ← ",
        Style::default().fg(if view.range > 0 { TEXT } else { MUTED }),
    );
    let right = Span::styled(
        " → ",
        Style::default().fg(if view.range + 1 < RANGES.len() {
            TEXT
        } else {
            MUTED
        }),
    );
    let selected = Style::default()
        .fg(color)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    let mut spans = vec![left.clone()];
    spans.extend(RANGE_NAMES.iter().enumerate().map(|(index, name)| {
        Span::styled(
            format!(" {name} "),
            if index == view.range {
                selected
            } else {
                Style::default().fg(MUTED)
            },
        )
    }));
    spans.push(right.clone());
    let choices = Line::from(spans);
    if choices.width() <= available {
        choices
    } else {
        Line::from(vec![
            left,
            Span::styled(RANGE_NAMES[view.range], selected),
            right,
        ])
    }
}

fn status_color(status: &str) -> Color {
    match status {
        "Charging" => BLUE,
        "Discharging" => ORANGE,
        "Full" => GREEN,
        "Not charging" => SECONDARY,
        _ => MUTED,
    }
}

fn metric(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    value: String,
    note: String,
    color: Color,
) {
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                value,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Line::styled(note, Style::default().fg(SECONDARY)),
        ])
        .block(panel(format!(" {title} "), BORDER)),
        area,
    );
}

fn metric_areas(area: Rect, compact: bool) -> Vec<Rect> {
    let mut cards = Vec::new();
    if compact {
        for row in Layout::vertical([Constraint::Length(4), Constraint::Length(4)])
            .split(area)
            .iter()
        {
            cards.extend(
                Layout::horizontal([Constraint::Percentage(50); 2])
                    .spacing(1)
                    .split(*row)
                    .iter()
                    .copied(),
            );
        }
    } else {
        cards.extend(
            Layout::horizontal([Constraint::Percentage(25); 4])
                .spacing(1)
                .split(area)
                .iter()
                .copied(),
        );
    }
    cards
}

fn draw_metrics(frame: &mut Frame<'_>, monitor: &Monitor, cards: &[Rect]) {
    let reading = monitor.reading();
    let color = accent(monitor);
    let charge = reading
        .and_then(|r| r.capacity_percent)
        .filter(|p| p.is_finite() && (0.0..=100.0).contains(p));
    let warning = match charge {
        Some(value) if value < 15.0 => Some(RED),
        Some(value) if value < 30.0 => Some(YELLOW),
        _ => None,
    };
    let energy = reading.and_then(|r| r.energy_now_wh);
    let energy_text = format!(
        "{}{}",
        if reading.is_some_and(|r| r.energy_estimated) {
            "~"
        } else {
            ""
        },
        number(energy, "Wh")
    );
    let mut percent = charge.map_or_else(|| "N/A".into(), |p| format!("{p:.1}%"));
    if percent.len() + 3 + energy_text.len() > cards[1].width.saturating_sub(2) as usize {
        percent = charge.map_or_else(|| "N/A".into(), |p| format!("{p:.0}%"));
    }
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                percent,
                Style::default()
                    .fg(warning.unwrap_or(TEXT))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" · {energy_text}"), Style::default().fg(SECONDARY)),
        ]))
        .block(panel(" Charge ", BORDER)),
        cards[1],
    );
    if let Some(charge) = charge {
        frame.render_widget(
            Gauge::default()
                .ratio((charge / 100.0).clamp(0.0, 1.0))
                .label("")
                .gauge_style(Style::default().fg(color).bg(BORDER)),
            Rect::new(
                cards[1].x + 1,
                cards[1].y + 2,
                cards[1].width.saturating_sub(2),
                1,
            ),
        );
    }
    let source = reading.map_or_else(
        || "No sensor".into(),
        |r| format!("Battery · {}", r.battery),
    );
    let power = number(reading.and_then(|r| r.power_w), "W");
    let status = reading.map_or("No battery", |r| r.status.as_str());
    let status = if status.len() + 1 + power.len() > cards[0].width.saturating_sub(2) as usize {
        match status {
            "Discharging" => "Disch.",
            "Charging" => "Charg.",
            "Not charging" => "No chg",
            other => other,
        }
    } else {
        status
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(status, Style::default().fg(color)),
                Span::raw(" "),
                Span::styled(
                    power,
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::styled(source, Style::default().fg(SECONDARY)),
        ])
        .block(panel(" Power ", BORDER)),
        cards[0],
    );
    let estimate = monitor.estimate();
    let (eta, note) = eta_text(
        monitor,
        estimate.as_ref(),
        cards[2].width.saturating_sub(2) as usize,
    );
    let charging = reading.is_some_and(|reading| {
        matches!(
            reading.status.as_str(),
            "Charging" | "Full" | "Not charging"
        )
    });
    let eta_color = if reading.is_some_and(|r| r.status == "Full") && !monitor.source_stale() {
        GREEN
    } else {
        TEXT
    };
    metric(
        frame,
        cards[2],
        if charging {
            "Time to full"
        } else {
            "Time to empty"
        },
        eta,
        note,
        eta_color,
    );
    let health = reading
        .and_then(|reading| reading.energy_full_wh.zip(reading.energy_design_wh))
        .filter(|(_, design)| *design > 0.0)
        .map(|(full, design)| full / design * 100.0);
    metric(
        frame,
        cards[3],
        "Battery health",
        number(health, "%"),
        format!(
            "Full {}",
            number(reading.and_then(|reading| reading.energy_full_wh), "Wh")
        ),
        TEXT,
    );
}

fn eta_text(monitor: &Monitor, estimate: Option<&Estimate>, available: usize) -> (String, String) {
    if monitor.source_stale() {
        return ("N/A".into(), "Stale reading".into());
    }
    let Some(reading) = monitor.reading() else {
        return ("N/A".into(), "No estimate available".into());
    };
    match reading.status.as_str() {
        "Full" => ("Full".into(), String::new()),
        "Not charging" => ("Not charging".into(), String::new()),
        "Charging" | "Discharging" => {
            if let (Some(estimate), Some(snapshot)) = (estimate, monitor.snapshot()) {
                return eta::format(estimate, snapshot.time.timestamp, available);
            }
            if reading.energy_now_wh.is_none() {
                ("N/A".into(), "Energy unavailable".into())
            } else if reading.status == "Charging" && reading.energy_full_wh.is_none() {
                ("N/A".into(), "Full size unknown".into())
            } else if reading.power_w == Some(0.0) {
                ("N/A".into(), "No measurable flow".into())
            } else {
                ("Estimating...".into(), "Need 60s of data".into())
            }
        }
        _ => ("N/A".into(), "No estimate available".into()),
    }
}

fn draw_details_popup(frame: &mut Frame<'_>, monitor: &Monitor) {
    let screen = frame.area();
    let width = screen.width.saturating_sub(4).min(40);
    let height = 11;
    let area = Rect::new(
        screen.right() - width - 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, area);
    draw_details(frame, monitor, area, true);
}

fn draw_details(frame: &mut Frame<'_>, monitor: &Monitor, area: Rect, popup: bool) {
    let reading = monitor.reading();
    let current = reading.and_then(current_value);
    let design = reading.and_then(|r| r.energy_design_wh);
    let voltage = reading.and_then(|r| r.voltage_v);
    let mut info = Vec::new();
    for (label, value, available) in [
        ("Design", number(design, "Wh"), design.is_some()),
        ("Voltage", number(voltage, "V"), voltage.is_some()),
        (
            "Current",
            current.map_or_else(
                || "N/A".into(),
                |(value, derived)| format!("{}{value:.2} A", if derived { "~" } else { "" }),
            ),
            current.is_some(),
        ),
    ] {
        info.push(Line::from(vec![
            Span::styled(format!(" {label:<9}"), Style::default().fg(SECONDARY)),
            Span::styled(
                value,
                Style::default().fg(if available { TEXT } else { MUTED }),
            ),
        ]));
    }
    if area.height >= 11 {
        info.push(Line::from(""));
    }
    if !monitor.live_enabled {
        info.push(Line::styled(
            " Power settings not saved",
            Style::default().fg(MUTED),
        ));
    } else {
        let settings = &monitor.power_settings;
        for (label, value) in [
            ("Platform", &settings.platform),
            ("Driver", &settings.driver),
            ("EPP", &settings.epp),
            ("Pstate", &settings.pstate),
            ("Governor", &settings.governor),
        ] {
            let display = match value.as_deref().unwrap_or("N/A") {
                "balance_performance" => "balance perf.",
                "balance_power" => "balance power",
                "balanced-performance" => "balanced perf.",
                other => other,
            };
            info.push(Line::from(vec![
                Span::styled(format!(" {label:<9}"), Style::default().fg(SECONDARY)),
                Span::styled(
                    display,
                    Style::default().fg(if value.is_some() { TEXT } else { MUTED }),
                ),
            ]));
        }
    }
    frame.render_widget(
        Paragraph::new(info).wrap(Wrap { trim: false }).block(panel(
            if popup {
                " Details · d / Esc to close "
            } else {
                " Details "
            },
            BORDER,
        )),
        area,
    );
}

fn recorder_warning(monitor: &Monitor) -> Option<&'static str> {
    if !monitor.live_enabled {
        return None;
    }
    let snapshot = monitor.recorded.as_ref()?;
    if snapshot.time.timestamp > monitor.now {
        Some("Save timestamp is ahead")
    } else if snapshot.is_stale(monitor.now) {
        Some("Recorder stale")
    } else {
        None
    }
}

fn draw_footer(frame: &mut Frame<'_>, monitor: &Monitor, area: Rect) {
    let mut brand = Vec::new();
    if !monitor.live_enabled {
        brand.push(Span::styled("HISTORY  ", Style::default().fg(YELLOW)));
    } else if monitor.source_stale() {
        brand.push(Span::styled("SENSOR STALE  ", Style::default().fg(YELLOW)));
    }
    brand.push(Span::styled(
        "den",
        Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
    ));
    let brand = Line::from(brand);
    let columns =
        Layout::horizontal([Constraint::Min(1), Constraint::Length(brand.width() as u16)])
            .spacing(1)
            .split(area);
    let keys = if columns[0].width >= 35 {
        vec![
            ("←→", " Window  "),
            ("↑↓", " Graph  "),
            ("?", " Help  "),
            ("q", " Quit"),
        ]
    } else {
        vec![("←→", " Window  "), ("?", " Help  "), ("q", " Quit")]
    };
    let controls = keys
        .into_iter()
        .flat_map(|(key, label)| {
            [
                Span::styled(key, Style::default().fg(TEXT)),
                Span::styled(label, Style::default().fg(SECONDARY)),
            ]
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(Line::from(controls)), columns[0]);
    frame.render_widget(
        Paragraph::new(brand).alignment(Alignment::Right),
        columns[1],
    );
}

fn current_value(reading: &BatteryReading) -> Option<(f64, bool)> {
    if let Some(current) = reading.current_a.filter(|v| v.is_finite() && *v >= 0.0) {
        return Some((current, false));
    }
    // Derive only for display: a missing CURRENT_NOW must stay missing in storage.
    let power = reading.power_w.filter(|v| v.is_finite() && *v >= 0.0)?;
    let voltage = reading.voltage_v.filter(|v| v.is_finite() && *v > 0.0)?;
    let current = power / voltage;
    current.is_finite().then_some((current, true))
}

fn number(value: Option<f64>, unit: &str) -> String {
    value.map_or_else(|| "N/A".into(), |value| format!("{value:.1} {unit}"))
}

struct Segment {
    points: Vec<(f64, f64)>,
    color: Color,
}

fn graph_segments<'a>(
    samples: impl Iterator<Item = &'a Sample>,
    end: i64,
    range: i64,
    graph: Graph,
    columns: usize,
) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut current = Vec::new();
    let mut previous: Option<&Sample> = None;
    let mut color = MUTED;
    for sample in samples {
        let value = match graph {
            Graph::Power => sample.reading.power_w,
            Graph::Charge => sample.reading.capacity_percent,
        }
        .filter(|value| {
            value.is_finite() && *value >= 0.0 && (graph != Graph::Charge || *value <= 100.0)
        });
        let in_range = (end - range..=end).contains(&sample.time.timestamp);
        let contiguous = previous.is_none_or(|previous| {
            sample.time.follows(
                &previous.time,
                sample.interval_secs.max(previous.interval_secs),
            ) && previous.reading.status == sample.reading.status
        });
        if (!contiguous || value.is_none() || !in_range) && !current.is_empty() {
            segments.push(Segment {
                points: downsample(&current, range, columns),
                color,
            });
            current.clear();
        }
        if let Some(value) = value.filter(|_| in_range) {
            current.push(((sample.time.timestamp - end) as f64, value));
            color = status_color(&sample.reading.status);
        }
        previous = Some(sample);
    }
    if !current.is_empty() {
        segments.push(Segment {
            points: downsample(&current, range, columns),
            color,
        });
    }
    segments
}

fn draw_chart(
    frame: &mut Frame<'_>,
    monitor: &Monitor,
    view: &View,
    graph: Graph,
    area: Rect,
    show_window: bool,
) {
    let range = RANGES[view.range];
    let segments = graph_segments(
        monitor.samples(),
        monitor.plot_end(),
        range,
        graph,
        area.width as usize,
    );
    let (minimum, maximum, step) = bounds(&segments, graph);
    let mut datasets = Vec::new();
    for segment in &segments {
        let color = segment.color;
        datasets.push(
            Dataset::default()
                .data(&segment.points)
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(color)),
        );
        // A single sample and the current endpoint must be visible, not a nearly
        // invisible subpixel at the edge of a many-hour window.
        if let Some(last) = segment.points.last() {
            datasets.push(
                Dataset::default()
                    .data(std::slice::from_ref(last))
                    .marker(Marker::Dot)
                    .graph_type(GraphType::Scatter)
                    .style(Style::default().fg(color)),
            );
        }
    }
    let title = match graph {
        Graph::Power => " Power · W ",
        Graph::Charge => " Charge · % ",
    };
    let color = accent(monitor);
    let mut block = panel(title, if graph == view.focus { color } else { BORDER });
    if show_window {
        let available = area
            .width
            .saturating_sub(Line::from(title).width() as u16 + 4) as usize;
        block = block.title_top(range_selector(view, color, available).right_aligned());
    }
    let intervals = ((maximum - minimum) / step).round() as usize;
    let stride = if area.height < 9 {
        if intervals.is_multiple_of(2) {
            intervals / 2
        } else {
            intervals
        }
    } else {
        1
    }
    .max(1);
    let labels = (0..=intervals)
        .step_by(stride)
        .map(|index| {
            let value = minimum + step * index as f64;
            let label = if step >= 1.0 {
                format!("{value:.0}")
            } else {
                format!("{value:.1}")
            };
            Line::styled(label, Style::default().fg(SECONDARY))
        })
        .collect::<Vec<_>>();
    let chart = Chart::new(datasets)
        .block(block)
        .x_axis(
            Axis::default()
                .bounds([-(range as f64), 0.0])
                .style(Style::default().fg(BORDER))
                .labels([
                    Line::styled(
                        format!("{} ago", RANGE_NAMES[view.range]),
                        Style::default().fg(SECONDARY),
                    ),
                    Line::styled("now", Style::default().fg(SECONDARY)),
                ]),
        )
        .y_axis(
            Axis::default()
                .bounds([minimum, maximum])
                .style(Style::default().fg(BORDER))
                .labels(labels),
        );
    frame.render_widget(chart, area);
    if segments.is_empty() && area.width > 35 && area.height > 5 {
        let text = if monitor.source_stale() && monitor.live_enabled {
            "Waiting for a live sensor"
        } else if monitor.reading().is_none() {
            "No battery samples"
        } else {
            "This sensor does not report this value"
        };
        frame.render_widget(
            Paragraph::new(text)
                .alignment(Alignment::Center)
                .style(Style::default().fg(MUTED)),
            Rect::new(
                area.x + 10,
                area.y + area.height / 2,
                area.width.saturating_sub(12),
                1,
            ),
        );
    }
}

fn bounds(segments: &[Segment], graph: Graph) -> (f64, f64, f64) {
    let mut values = segments
        .iter()
        .flat_map(|segment| segment.points.iter().map(|(_, value)| *value));
    let Some(first) = values.next() else {
        return if graph == Graph::Power {
            (0.0, 1.0, 0.5)
        } else {
            (0.0, 100.0, 25.0)
        };
    };
    let (min, max) = values.fold((first, first), |(min, max), value| {
        (min.min(value), max.max(value))
    });
    let (min, max) = if graph == Graph::Charge {
        let padding = ((max - min) * 0.15).max(2.0);
        ((min - padding).max(0.0), (max + padding).min(100.0))
    } else {
        (0.0, max.max(1.0))
    };
    if max - min < 1.0 {
        return (0.0, 1.0, 0.5);
    }
    let raw = (max - min) / 4.0;
    let scale = 10_f64.powf(raw.log10().floor());
    let factor = [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .find(|factor| factor * scale >= raw)
        .unwrap_or(10.0);
    let step = factor * scale;
    (
        (min / step).floor() * step,
        (max / step).ceil() * step,
        step,
    )
}

fn downsample(points: &[(f64, f64)], range: i64, columns: usize) -> Vec<(f64, f64)> {
    let bucket = |x: f64| ((x + range as f64) / range as f64 * columns.max(1) as f64) as usize;
    let mut result = Vec::new();
    let mut start = 0;
    while start < points.len() {
        let mut end = start + 1;
        while end < points.len() && bucket(points[end].0) == bucket(points[start].0) {
            end += 1;
        }
        let slice = &points[start..end];
        let min = (0..slice.len())
            .min_by(|a, b| slice[*a].1.total_cmp(&slice[*b].1))
            .unwrap_or(0);
        let max = (0..slice.len())
            .max_by(|a, b| slice[*a].1.total_cmp(&slice[*b].1))
            .unwrap_or(0);
        let mut indices = vec![0, min, max, slice.len() - 1];
        indices.sort_unstable();
        indices.dedup();
        result.extend(indices.into_iter().map(|index| slice[index]));
        start = end;
    }
    result
}

fn draw_help(frame: &mut Frame<'_>, monitor: &Monitor, view: &View, path: &Path) {
    let area = frame.area();
    let mut keys = vec![
        ("← →", "Time window"),
        ("↑ ↓", "Select graph"),
        ("f", "Expand / restore"),
        (
            "d",
            if sidebar_available(area.width, area.height, view.expanded) {
                "Toggle sidebar"
            } else {
                "Details"
            },
        ),
    ];
    if monitor.snapshot().is_some_and(|s| s.batteries.len() > 1) {
        keys.push(("b", "Next battery"));
    }
    keys.push(("q", "Quit"));
    let mut text = keys
        .into_iter()
        .map(|(key, action)| {
            Line::from(vec![
                Span::styled(format!(" {key:<7}"), Style::default().fg(TEXT)),
                Span::styled(action, Style::default().fg(SECONDARY)),
            ])
        })
        .collect::<Vec<_>>();
    let has_history_error = monitor.history_error.is_some() || monitor.usage_error.is_some();
    let recording_warning = recorder_warning(monitor);
    let has_errors =
        monitor.live_error.is_some() || has_history_error || recording_warning.is_some();
    if has_errors {
        text.push(Line::from(""));
        if has_history_error {
            text.push(Line::styled(
                format!("History: {}", path.display()),
                Style::default().fg(SECONDARY),
            ));
        }
        if let Some(estimate) = monitor.estimate() {
            text.push(Line::styled(
                format!(
                    "Prediction: {} (~{})",
                    estimate.method,
                    format_duration(estimate.seconds)
                ),
                Style::default().fg(SECONDARY),
            ));
        }
        if let Some(error) = &monitor.live_error {
            text.push(Line::styled(error.clone(), Style::default().fg(RED)));
        }
        if let Some(error) = &monitor.history_error {
            text.push(Line::styled(error.clone(), Style::default().fg(YELLOW)));
        }
        if let Some(warning) = recording_warning {
            text.push(Line::styled(warning, Style::default().fg(YELLOW)));
        }
        if let Some(error) = &monitor.usage_error
            && monitor.history_error.as_ref() != Some(error)
        {
            text.push(Line::styled(error.clone(), Style::default().fg(YELLOW)));
        }
    }
    let width = area
        .width
        .saturating_sub(6)
        .min(if has_errors { 80 } else { 34 });
    let inner_width = usize::from(width.saturating_sub(2)).max(1);
    let lines: usize = text
        .iter()
        .map(|line| {
            line.width().div_ceil(inner_width).max(1) + usize::from(line.width() > inner_width)
        })
        .sum();
    let height =
        (lines.saturating_add(2).min(u16::MAX as usize) as u16).min(area.height.saturating_sub(4));
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .block(panel(" Help · Esc to close ", accent(monitor))),
        popup,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BatteryReading, Snapshot, test_time};
    use ratatui::backend::TestBackend;

    fn reading(power: f64) -> BatteryReading {
        BatteryReading {
            battery: "BAT0".into(),
            status: "Discharging".into(),
            capacity_percent: Some(50.0),
            energy_now_wh: Some(25.0),
            energy_full_wh: Some(50.0),
            energy_design_wh: Some(55.0),
            power_w: Some(power),
            ..Default::default()
        }
    }

    fn text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn live_graph_is_visible_after_seconds_and_moves_without_a_database() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), true).unwrap();
        let view = View::default();
        assert_eq!(RANGES[view.range], 60);
        let mut terminal = Terminal::new(TestBackend::new(100, 32)).unwrap();
        monitor.remember_live(test_time(0), vec![reading(8.0)]);
        terminal
            .draw(|frame| draw(frame, &monitor, &view, Path::new("missing.db")))
            .unwrap();
        let first = terminal.backend().buffer().clone();
        assert!(text(&terminal).contains("Power · W"));
        assert!(text(&terminal).contains("Charge · %"));
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .any(|cell| cell.symbol() == "•")
        );
        for second in 1..=10 {
            monitor.remember_live(test_time(second), vec![reading(8.0 + second as f64)]);
        }
        terminal
            .draw(|frame| draw(frame, &monitor, &view, Path::new("missing.db")))
            .unwrap();
        assert_ne!(&first, terminal.backend().buffer());
        let plotted = graph_segments(monitor.samples(), monitor.plot_end(), 60, Graph::Power, 80);
        assert_eq!(plotted[0].points.len(), 11);
        let line_cells = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .filter(|cell| {
                cell.fg == ORANGE
                    && cell
                        .symbol()
                        .chars()
                        .any(|ch| ('\u{2801}'..='\u{28ff}').contains(&ch))
            })
            .count();
        assert!(
            line_cells >= 8,
            "live power trace should span multiple columns, got {line_cells}"
        );
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .any(|cell| cell.fg == TEXT)
        );
    }

    #[test]
    fn power_zero_is_drawn_not_hidden_and_is_labeled_as_battery_flow() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), true).unwrap();
        for second in 0..=5 {
            monitor.remember_live(test_time(second), vec![reading(0.0)]);
        }
        let mut terminal = Terminal::new(TestBackend::new(100, 32)).unwrap();
        terminal
            .draw(|frame| draw(frame, &monitor, &View::default(), Path::new("missing.db")))
            .unwrap();
        assert!(text(&terminal).contains("0.0 W"));
        assert!(text(&terminal).contains("Battery · BAT0"));
        let zero_trace_cells = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .filter(|cell| {
                cell.fg == ORANGE
                    && (cell.symbol() == "•"
                        || cell
                            .symbol()
                            .chars()
                            .any(|ch| ('\u{2801}'..='\u{28ff}').contains(&ch)))
            })
            .count();
        assert!(
            zero_trace_cells >= 3,
            "zero-watt trace must remain visible above the axis"
        );
        assert!(text(&terminal).contains("Discharging"));
        assert!(!text(&terminal).contains("-0.0 W"));
        let segments = graph_segments(monitor.samples(), monitor.plot_end(), 60, Graph::Power, 80);
        assert_eq!(bounds(&segments, Graph::Power), (0.0, 1.0, 0.5));
        assert_eq!(segments[0].points.len(), 6);
    }

    #[test]
    fn axes_include_zero_flow_and_keep_charge_in_percentage_bounds() {
        let power = vec![Segment {
            points: vec![(-1.0, 32.4), (0.0, 8.5)],
            color: GREEN,
        }];
        let (low, high, step) = bounds(&power, Graph::Power);
        assert_eq!(low, 0.0);
        assert!(high >= 32.4);
        assert_eq!((-low / step).fract(), 0.0);
        for percent in [0.0, 50.0, 100.0] {
            let charge = vec![Segment {
                points: vec![(0.0, percent)],
                color: TEXT,
            }];
            let (low, high, step) = bounds(&charge, Graph::Charge);
            assert!(low >= 0.0 && high <= 100.0 && low < high && step > 0.0);
            assert!(low <= percent && high >= percent);
        }
    }

    #[test]
    fn graph_preserves_peaks_and_does_not_bridge_suspend_or_missing_values() {
        let points = vec![(-10.0, 1.0), (-9.0, 20.0), (-8.0, 0.0), (-7.0, 2.0)];
        let reduced = downsample(&points, 60, 1);
        assert!(reduced.contains(&(-9.0, 20.0)));
        assert!(reduced.contains(&(-8.0, 0.0)));
        let mut samples: Vec<_> = [0, 1, 2]
            .into_iter()
            .map(|seconds| Sample {
                time: test_time(seconds),
                interval_secs: 1,
                reading: reading(8.0),
            })
            .collect();
        samples[2].time.boottime_ms += 10_000;
        let segments = graph_segments(samples.iter(), test_time(3).timestamp, 60, Graph::Power, 80);
        assert_eq!(segments.len(), 2);
        samples[2].time = test_time(2);
        samples[1].reading.power_w = None;
        assert_eq!(
            graph_segments(samples.iter(), test_time(3).timestamp, 60, Graph::Power, 80).len(),
            2
        );
    }

    #[test]
    fn renders_estimate_empty_stale_help_and_responsive_layouts() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), true).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(100, 32)).unwrap();
        for second in 0..=60 {
            monitor.remember_live(test_time(second), vec![reading(10.0)]);
        }
        let mut view = View::default();
        terminal
            .draw(|frame| draw(frame, &monitor, &view, Path::new("missing.db")))
            .unwrap();
        assert!(text(&terminal).contains("2h 05m – 3h 10m"));
        monitor.now += 10;
        terminal
            .draw(|frame| draw(frame, &monitor, &view, Path::new("missing.db")))
            .unwrap();
        assert!(text(&terminal).contains("SENSOR STALE"));
        assert!(!text(&terminal).contains("2h 05m – 3h 10m"));
        view.help = true;
        terminal
            .draw(|frame| draw(frame, &monitor, &view, Path::new("missing.db")))
            .unwrap();
        assert!(text(&terminal).contains("Select graph"));
        assert!(!text(&terminal).contains("What power means"));
        view.help = false;
        for (width, height) in [(20, 5), (48, 18), (60, 24), (80, 24), (120, 24), (140, 40)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| draw(frame, &monitor, &view, Path::new("missing.db")))
                .unwrap();
        }
        monitor.remember_live(test_time(80), vec![]);
        terminal
            .draw(|frame| draw(frame, &monitor, &view, Path::new("missing.db")))
            .unwrap();
        assert!(text(&terminal).contains("No battery"));
    }

    #[test]
    fn charging_uses_unsigned_flow_and_full_eta_without_repeated_status() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), true).unwrap();
        for second in 0..=60 {
            let mut charging = reading(20.0);
            charging.status = "Charging".into();
            monitor.remember_live(test_time(second), vec![charging]);
        }
        let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
        terminal
            .draw(|frame| draw(frame, &monitor, &View::default(), Path::new("missing.db")))
            .unwrap();
        let screen = text(&terminal);
        assert!(screen.contains("20.0 W"));
        assert!(!screen.contains("+20.0 W"));
        assert!(!screen.contains("-20.0 W"));
        assert!(screen.contains("Battery · BAT0"));
        assert!(!screen.contains("Battery power"));
        assert!(screen.contains("Time to full"));
        assert!(screen.contains("1h 00m – 1h 35m"));
        assert!(!screen.contains("Recent charge"));
        assert!(!screen.contains("Recent use"));
        assert!(!screen.contains("Time to empty"));
        assert!(!screen.contains("Not discharging"));
        assert!(!screen.contains("LIVE 1s"));
        assert!(!screen.contains("sensor: 1s"));
        assert_eq!(screen.matches("UTC").count(), 1); // Only usage, not a header clock.
        assert_eq!(screen.matches("Charging").count(), 1);
        let segments = graph_segments(monitor.samples(), monitor.plot_end(), 60, Graph::Power, 100);
        assert!(
            segments
                .iter()
                .all(|s| s.color == BLUE && s.points.iter().all(|(_, w)| *w > 0.0))
        );
        assert!(terminal.backend().buffer().content.iter().any(|cell| {
            cell.fg == BLUE
                && cell
                    .symbol()
                    .chars()
                    .any(|ch| ('\u{2801}'..='\u{28ff}').contains(&ch))
        }));
    }

    #[test]
    fn both_columns_share_the_power_charge_boundary_with_usage_above_details() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), true).unwrap();
        monitor.remember_live(test_time(0), vec![reading(10.0)]);
        for width in [80, 119, 120, 121, 140, 180, 201, 240] {
            let mut terminal = Terminal::new(TestBackend::new(width, 40)).unwrap();
            terminal
                .draw(|frame| draw(frame, &monitor, &View::default(), Path::new("missing.db")))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let charge_y = (7..39)
                .find(|y| {
                    (0..width)
                        .map(|x| buffer[(x, *y)].symbol())
                        .collect::<String>()
                        .contains("Charge · %")
                })
                .unwrap();
            let cards = metric_areas(Rect::new(0, 0, width, 5), false);
            let right = if width >= 120 {
                cards[2].right() - 1
            } else {
                width - 1
            };
            for y in [5, charge_y] {
                assert_eq!(buffer[(0, y)].symbol(), "╭");
                assert_eq!(buffer[(right, y)].symbol(), "╮");
            }
            if width >= 120 {
                assert_eq!(buffer[(cards[3].x, 5)].symbol(), "╭");
                assert_eq!(buffer[(cards[3].x, charge_y - 2)].symbol(), "╰");
                assert_eq!(buffer[(cards[3].x, charge_y)].symbol(), "╭");
                assert_eq!(buffer[(cards[3].x, 37)].symbol(), "╰");
                assert!((9..=12).contains(&(38 - charge_y)));
                assert!(text(&terminal).contains("Battery use · Wh"));
                let details_title = (cards[3].x..width)
                    .map(|x| buffer[(x, charge_y)].symbol())
                    .collect::<String>();
                assert!(details_title.contains("Details"));
                assert!(text(&terminal).contains("55.0 Wh"));
            } else {
                assert!(!text(&terminal).contains("55.0 Wh"));
                assert!(!text(&terminal).contains("Battery use · Wh"));
            }
            let power = (0..width)
                .map(|x| buffer[(x, 5)].symbol())
                .collect::<String>();
            let charge = (0..width)
                .map(|x| buffer[(x, charge_y)].symbol())
                .collect::<String>();
            assert!(power.contains("Power · W"));
            assert!(charge.contains("Charge · %"));
            assert!(!text(&terminal).contains("Details ·"));
            let top = (0..width)
                .map(|x| buffer[(x, 0)].symbol())
                .collect::<String>();
            assert!(top.find("Power").unwrap() < top.find("Charge").unwrap());
            assert!(top.find("Charge").unwrap() < top.find("Time to empty").unwrap());
            assert!(top.find("Time to empty").unwrap() < top.find("Battery health").unwrap());
            assert_eq!(buffer[(width - 3, 39)].symbol(), "d");
        }
    }

    #[test]
    fn eta_pairs_a_white_duration_with_a_secondary_clock_range_and_full_stays_green() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), true).unwrap();
        for (status, start) in [("Charging", 0), ("Discharging", 301)] {
            for second in start..=start + 300 {
                let mut current = reading(20.0);
                current.status = status.into();
                monitor.remember_live(test_time(second), vec![current]);
            }
            for width in [48, 72, 100, 140] {
                let compact = width < 72;
                let card =
                    metric_areas(Rect::new(0, 0, width, if compact { 8 } else { 4 }), compact)[2];
                let mut terminal = Terminal::new(TestBackend::new(width, 32)).unwrap();
                terminal
                    .draw(|frame| draw(frame, &monitor, &View::default(), Path::new("missing.db")))
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let row = |y| {
                    (card.x + 1..card.right() - 1)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                };
                assert!(row(card.y + 1).contains('–'));
                assert!(row(card.y + 2).contains(':'));
                let clock_start = (card.x + 1..card.right() - 1)
                    .find(|x| buffer[(*x, card.y + 2)].symbol() != " ")
                    .unwrap();
                assert_eq!(buffer[(clock_start, card.y + 2)].fg, SECONDARY);
                assert!(
                    (card.x + 1..card.right() - 1)
                        .all(|x| !blocks_contain(buffer[(x, card.y + 1)].symbol()))
                );
                let value = &buffer[(card.x + 1, card.y + 1)];
                assert_eq!(value.fg, TEXT);
                assert!(value.modifier.contains(Modifier::BOLD));
                assert!(
                    value.symbol() == "I" || value.symbol().chars().all(|c| c.is_ascii_digit())
                );
            }
        }
        let mut full = reading(0.0);
        full.status = "Full".into();
        full.capacity_percent = Some(100.0);
        full.energy_now_wh = full.energy_full_wh;
        monitor.remember_live(test_time(602), vec![full]);
        let mut terminal = Terminal::new(TestBackend::new(100, 32)).unwrap();
        let card = metric_areas(Rect::new(0, 0, 100, 4), false)[2];
        terminal
            .draw(|frame| draw(frame, &monitor, &View::default(), Path::new("missing.db")))
            .unwrap();
        assert_eq!(terminal.backend().buffer()[(card.x + 1, 1)].symbol(), "F");
        assert_eq!(terminal.backend().buffer()[(card.x + 1, 1)].fg, GREEN);
        monitor.now += 10;
        terminal
            .draw(|frame| draw(frame, &monitor, &View::default(), Path::new("missing.db")))
            .unwrap();
        assert_eq!(terminal.backend().buffer()[(card.x + 1, 1)].symbol(), "N");
        assert_eq!(terminal.backend().buffer()[(card.x + 1, 1)].fg, TEXT);
        assert!(text(&terminal).contains("Stale reading"));
    }

    #[test]
    fn available_eta_uses_the_reading_time_not_the_viewers_clock() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), true).unwrap();
        for (status, start) in [("Charging", 39), ("Discharging", 159)] {
            for second in start..=start + 60 {
                let mut current = reading(20.0);
                current.status = status.into();
                monitor.remember_live(test_time(second), vec![current]);
            }
            let estimate = monitor.estimate().unwrap();
            let base = monitor.snapshot().unwrap().time.timestamp;
            monitor.now = base + 2;
            let (range, arrival) = eta_text(&monitor, Some(&estimate), 48);
            assert!(range.starts_with("In  ") && range.contains('–'));
            assert!(arrival.starts_with("At  ") && arrival.contains(':'));
            let expected = eta::format(&estimate, base, 48);
            assert_eq!((range, arrival), expected);
            assert_ne!(expected.1, eta::format(&estimate, monitor.now, 48).1);
            monitor.recorded = monitor.snapshot().cloned();
            monitor.live_enabled = false;
            assert_eq!(eta_text(&monitor, Some(&estimate), 48), expected);
            monitor.live_enabled = true;
        }
    }

    #[test]
    fn charging_eta_distinguishes_warmup_missing_capacity_not_charging_and_full() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), true).unwrap();
        let mut charging = reading(20.0);
        charging.status = "Charging".into();
        monitor.remember_live(test_time(0), vec![charging.clone()]);
        assert_eq!(eta_text(&monitor, None, 30).0, "Estimating...");
        charging.energy_full_wh = None;
        monitor.remember_live(test_time(1), vec![charging.clone()]);
        assert_eq!(eta_text(&monitor, None, 30).1, "Full size unknown");
        charging.status = "Not charging".into();
        monitor.remember_live(test_time(2), vec![charging.clone()]);
        assert_eq!(eta_text(&monitor, None, 30).0, "Not charging");
        charging.status = "Full".into();
        monitor.remember_live(test_time(3), vec![charging]);
        assert_eq!(eta_text(&monitor, None, 30).0, "Full");
    }

    #[test]
    fn not_charging_never_implies_a_pause_or_full_regardless_of_charge_level() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), true).unwrap();
        let previous_estimate = Estimate {
            seconds: 1_800,
            lower_seconds: 1_200,
            upper_seconds: Some(2_400),
            method: "smoothed power",
        };
        for (second, percent) in [60.0, 80.0, 100.0].into_iter().enumerate() {
            let mut stopped = reading(0.0);
            stopped.status = "Not charging".into();
            stopped.external_power = Some(true);
            stopped.capacity_percent = Some(percent);
            stopped.energy_full_wh = Some(50.0);
            stopped.energy_now_wh = Some(percent / 100.0 * 50.0);
            monitor.remember_live(test_time(second as i64), vec![stopped]);
            assert_eq!(
                eta_text(&monitor, Some(&previous_estimate), 30),
                ("Not charging".into(), String::new())
            );
        }
    }

    #[test]
    fn both_histories_keep_three_distinct_state_colors() {
        let samples: Vec<_> = [
            ("Discharging", 8.0, 60.0),
            ("Charging", 24.0, 70.0),
            ("Full", 0.0, 100.0),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (status, power, percent))| {
            let mut battery = reading(power);
            battery.status = status.into();
            battery.capacity_percent = Some(percent);
            Sample {
                time: test_time(index as i64),
                interval_secs: 1,
                reading: battery,
            }
        })
        .collect();
        for graph in [Graph::Power, Graph::Charge] {
            let segments = graph_segments(samples.iter(), test_time(2).timestamp, 60, graph, 80);
            assert_eq!(
                segments.iter().map(|s| s.color).collect::<Vec<_>>(),
                [ORANGE, BLUE, GREEN]
            );
            assert!(segments.iter().all(|s| s.points.len() == 1));
        }
    }

    #[test]
    fn unsigned_history_preserves_state_colors_and_breaks_at_transitions() {
        let mut samples: Vec<_> = (0..6)
            .map(|second| Sample {
                time: test_time(second),
                interval_secs: 1,
                reading: reading(8.0),
            })
            .collect();
        for sample in &mut samples[3..] {
            sample.reading.status = "Charging".into();
            sample.reading.power_w = Some(24.0);
        }
        let segments = graph_segments(samples.iter(), test_time(5).timestamp, 60, Graph::Power, 80);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].color, ORANGE);
        assert_eq!(segments[1].color, BLUE);
        assert_eq!(segments[0].points.len(), 3);
        assert_eq!(segments[1].points.len(), 3);
        assert!(
            segments
                .iter()
                .flat_map(|s| &s.points)
                .all(|(_, w)| *w >= 0.0)
        );
        for invalid in [f64::NAN, -1.0] {
            samples[1].reading.power_w = Some(invalid);
            assert_eq!(
                graph_segments(samples.iter(), test_time(5).timestamp, 60, Graph::Power, 80).len(),
                3
            );
        }
    }

    #[test]
    fn grayscale_metrics_and_footer_keep_state_accents_and_warning_precedence() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), true).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
        for (second, (status, expected)) in [
            ("Discharging", ORANGE),
            ("Charging", BLUE),
            ("Full", GREEN),
            ("Not charging", SECONDARY),
            ("Unknown", MUTED),
        ]
        .into_iter()
        .enumerate()
        {
            let mut current = reading(12.0);
            current.status = status.into();
            current.capacity_percent = Some(10.0);
            if status == "Not charging" {
                current.power_w = Some(0.0);
            }
            monitor.remember_live(test_time(second as i64), vec![current]);
            assert_eq!(accent(&monitor), expected);
            terminal
                .draw(|frame| draw(frame, &monitor, &View::default(), Path::new("missing.db")))
                .unwrap();
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(0, 5)].fg, expected);
            assert_eq!(buffer[(137, 39)].fg, TEXT);
            let cards = metric_areas(Rect::new(0, 0, 140, 4), false);
            assert_eq!(buffer[(cards[3].x + 1, 1)].fg, TEXT);
            assert_eq!(buffer[(cards[1].x + 1, 1)].fg, RED);
            assert_eq!(buffer[(cards[0].x + 1, 1)].fg, expected);
            assert_eq!(buffer[(cards[0].x + 2 + status.len() as u16, 1)].fg, TEXT);
            assert_eq!(
                buffer[(cards[2].x + 1, 1)].fg,
                if status == "Full" { GREEN } else { TEXT }
            );
            assert_eq!(buffer[(cards[0].x + 1, 2)].fg, SECONDARY);
            assert_eq!(buffer[(0, 39)].fg, TEXT);
            assert!(
                buffer
                    .content
                    .iter()
                    .any(|cell| cell.symbol() == "█" && cell.fg == expected)
            );
            assert!(buffer.content.iter().any(|cell| cell.modifier.contains(Modifier::UNDERLINED) && cell.fg == expected));
            assert!(text(&terminal).contains(if status == "Not charging" {
                "0.0 W"
            } else {
                "12.0 W"
            }));
            if status == "Not charging" {
                assert!(text(&terminal).contains("Not charging"));
            }
            assert!(!text(&terminal).contains("-12.0 W"));
        }
        monitor.now += 10;
        assert_eq!(accent(&monitor), YELLOW);
    }

    #[test]
    fn window_selector_is_in_the_chart_and_follows_expansion_and_narrow_layouts() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), true).unwrap();
        monitor.remember_live(test_time(0), vec![reading(12.0)]);
        for (width, height, focus, expanded) in [
            (140, 40, Graph::Power, false),
            (140, 40, Graph::Charge, true),
            (80, 24, Graph::Power, false),
            (48, 18, Graph::Charge, false),
        ] {
            let view = View {
                focus,
                expanded,
                ..Default::default()
            };
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| draw(frame, &monitor, &view, Path::new("missing.db")))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let selector_row = if width < 72 { 9 } else { 5 };
            let line = (0..width)
                .map(|x| buffer[(x, selector_row)].symbol())
                .collect::<String>();
            assert!(line.contains("60s"));
            assert!(line.contains(if focus == Graph::Charge {
                "Charge · %"
            } else {
                "Power · W"
            }));
            assert_eq!(text(&terminal).matches("den").count(), 1);
            if width == 48 {
                assert!(line.contains("← 60s →"));
            }
            if expanded {
                assert!(!text(&terminal).contains("Power · W"));
                assert!(!text(&terminal).contains("Details ·"));
            }
        }
    }

    #[test]
    fn charge_history_and_its_endpoint_share_the_power_state_color() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), true).unwrap();
        for second in 0..=10 {
            let mut current = reading(10.0);
            current.status = "Discharging".into();
            current.capacity_percent = Some(85.0);
            monitor.remember_live(test_time(second), vec![current]);
        }
        let segments = graph_segments(
            monitor.samples(),
            monitor.plot_end(),
            60,
            Graph::Charge,
            100,
        );
        assert!(segments.iter().all(|segment| segment.color == ORANGE));
        let mut terminal = Terminal::new(TestBackend::new(100, 15)).unwrap();
        terminal
            .draw(|frame| {
                draw_chart(
                    frame,
                    &monitor,
                    &View::default(),
                    Graph::Charge,
                    frame.area(),
                    false,
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert!(
            buffer
                .content
                .iter()
                .any(|cell| cell.symbol() == "•" && cell.fg == ORANGE)
        );
        assert!(buffer.content.iter().any(|cell| {
            cell.fg == ORANGE
                && cell
                    .symbol()
                    .chars()
                    .any(|ch| ('\u{2801}'..='\u{28ff}').contains(&ch))
        }));
        monitor.now += 10;
        terminal
            .draw(|frame| {
                draw_chart(
                    frame,
                    &monitor,
                    &View::default(),
                    Graph::Charge,
                    frame.area(),
                    false,
                )
            })
            .unwrap();
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .any(|cell| cell.symbol() == "•" && cell.fg == ORANGE)
        );
        assert!(!text(&terminal).contains("auto scale"));
    }

    #[test]
    fn charge_history_keeps_original_state_colors_instead_of_remaining_level_colors() {
        let samples: Vec<_> = [
            ("Discharging", 95.0),
            ("Discharging", 85.0),
            ("Charging", 30.0),
            ("Charging", 40.0),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (status, percent))| {
            let mut battery = reading(12.0);
            battery.status = status.into();
            battery.capacity_percent = Some(percent);
            Sample {
                time: test_time(index as i64 * 30),
                interval_secs: 30,
                reading: battery,
            }
        })
        .collect();
        let segments = graph_segments(
            samples.iter(),
            test_time(90).timestamp,
            120,
            Graph::Charge,
            100,
        );
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].color, ORANGE);
        assert_eq!(segments[1].color, BLUE);
        assert_eq!(segments[0].points.len(), 2);
        assert_eq!(segments[1].points.len(), 2);
    }

    #[test]
    fn charge_history_does_not_bridge_missing_or_invalid_remaining_levels() {
        for invalid in [None, Some(-1.0), Some(101.0), Some(f64::NAN)] {
            let samples: Vec<_> = [Some(85.0), invalid, Some(83.0)]
                .into_iter()
                .enumerate()
                .map(|(index, percent)| {
                    let mut battery = reading(12.0);
                    battery.capacity_percent = percent;
                    Sample {
                        time: test_time(index as i64 * 30),
                        interval_secs: 30,
                        reading: battery,
                    }
                })
                .collect();
            let segments = graph_segments(
                samples.iter(),
                test_time(60).timestamp,
                60,
                Graph::Charge,
                100,
            );
            assert_eq!(segments.len(), 2);
            assert!(
                segments
                    .iter()
                    .all(|s| s.color == ORANGE && s.points.len() == 1)
            );
        }
    }

    #[test]
    fn recording_is_hidden_in_details_but_problems_remain_in_help() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), true).unwrap();
        monitor.recorded = Some(Snapshot {
            frame_id: 1,
            time: test_time(0),
            interval_secs: 30,
            batteries: vec![reading(12.0)],
        });
        monitor.remember_live(test_time(100), vec![reading(12.0)]);
        assert_eq!(recorder_warning(&monitor), Some("Recorder stale"));
        let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
        terminal
            .draw(|frame| draw(frame, &monitor, &View::default(), Path::new("missing.db")))
            .unwrap();
        for removed in ["Recording", "Last saved", "Every 30s", "Recorder stale"] {
            assert!(!text(&terminal).contains(removed));
        }
        let view = View {
            help: true,
            ..Default::default()
        };
        terminal
            .draw(|frame| draw(frame, &monitor, &view, Path::new("missing.db")))
            .unwrap();
        assert!(text(&terminal).contains("Recorder stale"));
        monitor.live_enabled = false;
        assert_eq!(recorder_warning(&monitor), None);
    }

    #[test]
    fn charge_gauge_follows_state_and_all_summary_cards_have_two_content_rows() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), true).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(100, 32)).unwrap();
        for status in ["Charging", "Discharging", "Full", "Not charging", "Unknown"] {
            for percent in [10.0, 25.0, 50.0, 85.0, 100.0] {
                let expected = status_color(status);
                let mut current = reading(12.0);
                current.status = status.into();
                current.capacity_percent = Some(percent);
                monitor.remember_live(test_time(0), vec![current]);
                terminal
                    .draw(|frame| draw(frame, &monitor, &View::default(), Path::new("missing.db")))
                    .unwrap();
                let buffer = terminal.backend().buffer();
                assert!(
                    buffer
                        .content
                        .iter()
                        .any(|cell| cell.symbol() == "█" && cell.fg == expected)
                );
                let cards = metric_areas(Rect::new(0, 0, 100, 4), false);
                for card in &cards {
                    assert_eq!(buffer[(card.x, 3)].symbol(), "╰");
                }
                assert_eq!(
                    buffer[(cards[1].x + 1, 1)].fg,
                    if percent < 15.0 {
                        RED
                    } else if percent < 30.0 {
                        YELLOW
                    } else {
                        TEXT
                    }
                );
                assert_eq!(buffer[(cards[1].x + 1, 2)].fg, expected);
            }
        }
    }

    #[test]
    fn narrow_details_overlay_closes_without_rearranging_the_graphs() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), true).unwrap();
        monitor.remember_live(test_time(0), vec![reading(10.0)]);
        for (width, height) in [(48, 18), (80, 24)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut view = View::default();
            terminal
                .draw(|frame| draw(frame, &monitor, &view, Path::new("missing.db")))
                .unwrap();
            let original = terminal.backend().buffer().clone();
            view.details = Some(true);
            terminal
                .draw(|frame| draw(frame, &monitor, &view, Path::new("missing.db")))
                .unwrap();
            assert!(text(&terminal).contains("Details · d / Esc to close"));
            assert!(text(&terminal).contains("55.0 Wh"));
            assert!(!text(&terminal).contains("Recording"));
            assert!(!text(&terminal).contains("Last saved"));
            view.details = Some(false);
            terminal
                .draw(|frame| draw(frame, &monitor, &view, Path::new("missing.db")))
                .unwrap();
            assert_eq!(&original, terminal.backend().buffer());
        }
    }

    #[test]
    fn details_can_be_explicitly_hidden_or_shown_and_resizing_is_automatic_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), true).unwrap();
        monitor.remember_live(test_time(0), vec![reading(10.0)]);
        for (width, choice, visible) in [
            (100, None, false),
            (140, None, true),
            (140, Some(false), false),
            (140, Some(true), true),
            (100, Some(true), true),
        ] {
            let view = View {
                details: choice,
                ..Default::default()
            };
            let mut terminal = Terminal::new(TestBackend::new(width, 40)).unwrap();
            terminal
                .draw(|frame| draw(frame, &monitor, &view, Path::new("missing.db")))
                .unwrap();
            assert_eq!(text(&terminal).contains("55.0 Wh"), visible);
            let right = if visible && width >= 120 {
                metric_areas(Rect::new(0, 0, width, 5), false)[2].right() - 1
            } else {
                width - 1
            };
            assert_eq!(terminal.backend().buffer()[(right, 5)].symbol(), "╮");
        }
    }

    #[test]
    fn blue_tinted_secondary_text_and_footer_remain_bright() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), true).unwrap();
        monitor.remember_live(test_time(0), vec![reading(10.0)]);
        let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
        terminal
            .draw(|frame| draw(frame, &monitor, &View::default(), Path::new("missing.db")))
            .unwrap();
        let cards = metric_areas(Rect::new(0, 0, 140, 5), false);
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(cards[0].x + 1, 2)].fg, SECONDARY);
        assert_eq!(buffer[(cards[3].x + 1, 2)].fg, SECONDARY);
        assert_eq!(buffer[(0, 39)].fg, TEXT);
        assert_eq!(buffer[(4, 39)].fg, SECONDARY);
        assert_eq!(buffer[(cards[3].x + 2, 6)].fg, SECONDARY);
    }

    #[test]
    fn hourly_bars_are_white_and_distinguish_missing_zero_partial_and_current_data() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), true).unwrap();
        let mut current = reading(20.0);
        current.status = "Charging".into();
        monitor.remember_live(test_time(0), vec![current]);
        let mut usage = crate::usage::UsageHistory::new(monitor.now);
        for (index, energy, seconds) in [
            (0, 2.5, HOUR),
            (6, 0.0, HOUR),
            (12, 1.0, 1_800),
            (23, 0.5, 600),
        ] {
            usage.hours[index].energy_wh = energy;
            usage.hours[index].covered_seconds = seconds;
        }
        monitor.usage = Some(usage);
        for (width, height) in [(120, 24), (120, 40), (140, 40), (200, 50)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| draw(frame, &monitor, &View::default(), Path::new("missing.db")))
                .unwrap();
            assert!(text(&terminal).contains("Observed 4.0 Wh"));
            if height == 24 {
                assert!(text(&terminal).contains("Enlarge for hourly bars"));
                continue;
            }
            let buffer = terminal.backend().buffer();
            let left = metric_areas(Rect::new(0, 0, width, 5), false)[3].x + 1;
            let inner_width = width - left - 1;
            let legend_y = (0..height)
                .find(|y| {
                    (left..width)
                        .map(|x| buffer[(x, *y)].symbol())
                        .collect::<String>()
                        .contains("* now")
                })
                .unwrap();
            for (index, marker) in [(1, "·"), (6, "0"), (12, "~"), (23, "*")] {
                let x = left + index * inner_width / HOURS as u16;
                assert_eq!(buffer[(x, legend_y - 2)].symbol(), marker);
            }
            let mut bars = 0;
            for y in 10..legend_y - 2 {
                for x in left..width - 1 {
                    let cell = &buffer[(x, y)];
                    if blocks_contain(cell.symbol()) {
                        assert_eq!(cell.fg, TEXT);
                        bars += 1;
                    }
                }
            }
            assert!(bars > 0);
        }
    }

    #[test]
    fn small_recorded_amounts_remain_distinct_from_zero() {
        assert_eq!(usage_number(0.0), "0.0");
        assert_eq!(usage_number(0.05), "0.050");
        assert_eq!(usage_number(0.0083), "0.0083");
        assert_eq!(usage_number(0.00001), "1.0e-5");
        assert_eq!(usage_number(12.45), "12.4");
    }

    fn blocks_contain(symbol: &str) -> bool {
        ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"].contains(&symbol)
    }

    #[test]
    fn usage_without_consecutive_measurements_is_not_presented_as_zero() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), true).unwrap();
        monitor.remember_live(test_time(0), vec![reading(8.0)]);
        let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
        terminal
            .draw(|frame| draw(frame, &monitor, &View::default(), Path::new("missing.db")))
            .unwrap();
        assert!(text(&terminal).contains("No saved history"));
        monitor.usage = Some(crate::usage::UsageHistory::new(monitor.now));
        terminal
            .draw(|frame| draw(frame, &monitor, &View::default(), Path::new("missing.db")))
            .unwrap();
        assert!(text(&terminal).contains("Observed N/A"));
        assert!(!text(&terminal).contains("Observed 0.0 Wh"));
        assert!(text(&terminal).contains("· no data"));
    }

    #[test]
    fn power_card_uses_two_lines_and_preserves_the_value_on_narrow_terminals() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), true).unwrap();
        for width in [48, 60, 72, 76, 80, 100, 140] {
            for watts in [7.3, 1_000.0] {
                monitor.remember_live(test_time(0), vec![reading(watts)]);
                let compact = width < 72;
                let cards =
                    metric_areas(Rect::new(0, 0, width, if compact { 8 } else { 5 }), compact);
                let card = cards[0];
                let mut terminal = Terminal::new(TestBackend::new(width, 32)).unwrap();
                terminal
                    .draw(|frame| draw(frame, &monitor, &View::default(), Path::new("missing.db")))
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let row = |y| {
                    (card.x + 1..card.right() - 1)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                };
                let first = row(card.y + 1);
                assert!(
                    first.contains(&number(Some(watts), "W")),
                    "{width}: {first}"
                );
                assert!(first.starts_with("Discharging ") || first.starts_with("Disch. "));
                assert_eq!(row(card.y + 2).trim(), "Battery · BAT0");
                assert_eq!(buffer[(card.x, card.y + 3)].symbol(), "╰");
            }
        }
    }

    #[test]
    fn details_identify_live_controls_and_do_not_project_them_onto_history() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), true).unwrap();
        monitor.remember_live(test_time(0), vec![reading(10.0)]);
        monitor.power_settings = crate::power::PowerSettings {
            platform: Some("balanced".into()),
            driver: Some("intel_pstate".into()),
            pstate: Some("active".into()),
            epp: Some("balance_performance".into()),
            governor: Some("powersave".into()),
        };
        for (width, height) in [(120, 24), (140, 40)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| draw(frame, &monitor, &View::default(), Path::new("missing.db")))
                .unwrap();
            for value in [
                "Platform balanced",
                "Driver   intel_pstate",
                "EPP      balance perf.",
                "Pstate   active",
            ] {
                assert!(text(&terminal).contains(value), "{width}x{height}: {value}");
            }
            if height == 40 {
                assert!(text(&terminal).contains("Governor powersave"));
            }
            monitor.live_enabled = false;
            terminal
                .draw(|frame| draw(frame, &monitor, &View::default(), Path::new("missing.db")))
                .unwrap();
            assert!(text(&terminal).contains("Power settings not saved"));
            for value in [
                "balanced",
                "intel_pstate",
                "balance perf.",
                "powersave",
                "active",
            ] {
                assert!(!text(&terminal).contains(value));
            }
            monitor.live_enabled = true;
        }
    }

    #[test]
    fn displayed_current_prefers_measured_values_and_only_derives_from_valid_watts_and_volts() {
        let mut battery = reading(7.3);
        battery.voltage_v = Some(12.5);
        assert_eq!(current_value(&battery), Some((7.3 / 12.5, true)));
        assert_eq!(battery.current_a, None);
        for current in [0.0, 0.42] {
            battery.current_a = Some(current);
            assert_eq!(current_value(&battery), Some((current, false)));
        }
        battery.current_a = None;
        for voltage in [
            None,
            Some(0.0),
            Some(-1.0),
            Some(f64::NAN),
            Some(f64::INFINITY),
        ] {
            battery.voltage_v = voltage;
            assert_eq!(current_value(&battery), None);
        }
        battery.voltage_v = Some(12.5);
        for power in [None, Some(-1.0), Some(f64::NAN), Some(f64::INFINITY)] {
            battery.power_w = power;
            assert_eq!(current_value(&battery), None);
        }
        battery.power_w = Some(0.0);
        assert_eq!(current_value(&battery), Some((0.0, true)));
        battery.power_w = Some(f64::MAX);
        battery.voltage_v = Some(f64::MIN_POSITIVE);
        assert_eq!(current_value(&battery), None);
    }

    #[test]
    fn details_mark_calculated_current_and_leave_raw_measurements_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), true).unwrap();
        let mut battery = reading(7.3);
        battery.voltage_v = Some(12.5);
        let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
        for (current, expected) in [
            (None, "~0.58 A"),
            (Some(0.0), "0.00 A"),
            (Some(0.42), "0.42 A"),
        ] {
            battery.current_a = current;
            monitor.remember_live(test_time(0), vec![battery.clone()]);
            terminal
                .draw(|frame| draw(frame, &monitor, &View::default(), Path::new("missing.db")))
                .unwrap();
            assert!(text(&terminal).contains(&format!("Current  {expected}")));
            assert_eq!(monitor.reading().unwrap().current_a, current);
        }
    }

    #[test]
    fn help_is_a_small_contextual_key_table_with_diagnostics_only_on_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), true).unwrap();
        monitor.remember_live(test_time(0), vec![reading(7.3)]);
        let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
        let view = View {
            help: true,
            ..Default::default()
        };
        terminal
            .draw(|frame| draw(frame, &monitor, &view, Path::new("missing.db")))
            .unwrap();
        let screen = text(&terminal);
        for action in [
            "Time window",
            "Select graph",
            "Expand / restore",
            "Toggle sidebar",
            "Quit",
        ] {
            assert!(screen.contains(action));
        }
        for unused in [
            "Next battery",
            "Live monitoring",
            "Prediction:",
            "History:",
            "What power means",
            "Tab:",
            "reload",
        ] {
            assert!(!screen.contains(unused));
        }
        let buffer = terminal.backend().buffer();
        let help_row = (0..40)
            .find(|y| {
                (0..140)
                    .map(|x| buffer[(x, *y)].symbol())
                    .collect::<String>()
                    .contains("Help · Esc")
            })
            .unwrap();
        let left = (0..140)
            .find(|x| buffer[(*x, help_row)].symbol() == "╭")
            .unwrap();
        assert_eq!(buffer[(left, help_row + 6)].symbol(), "╰");
        assert!(left > 50, "normal help should be narrow and centered");

        let mut second = reading(7.3);
        second.battery = "BAT1".into();
        monitor.remember_live(test_time(0), vec![reading(7.3), second]);
        monitor.live_error = Some("Sensor unavailable: fixture disconnected".into());
        monitor.history_error = Some("History unavailable: busy fixture".into());
        monitor.usage_error = monitor.history_error.clone();
        terminal
            .draw(|frame| draw(frame, &monitor, &view, Path::new("missing.db")))
            .unwrap();
        let screen = text(&terminal);
        assert!(screen.contains("Next battery"));
        assert!(screen.contains("Sensor unavailable: fixture disconnected"));
        assert_eq!(
            screen.matches("History unavailable: busy fixture").count(),
            1
        );
        assert!(screen.contains("History: missing.db"));
    }

    #[test]
    fn selector_arrows_show_available_directions_in_full_and_compact_forms() {
        for (range, left, right) in [(0, MUTED, TEXT), (3, TEXT, TEXT), (6, TEXT, MUTED)] {
            let view = View {
                range,
                ..Default::default()
            };
            for available in [100, 15] {
                let line = range_selector(&view, ORANGE, available);
                assert_eq!(line.spans.first().unwrap().content, " ← ");
                assert_eq!(line.spans.first().unwrap().style.fg, Some(left));
                assert_eq!(line.spans.last().unwrap().content, " → ");
                assert_eq!(line.spans.last().unwrap().style.fg, Some(right));
                assert!(line.width() <= available);
                assert!(
                    line.spans
                        .iter()
                        .any(|span| span.content.contains(RANGE_NAMES[range])
                            && span.style.fg == Some(ORANGE))
                );
            }
        }
    }

    #[test]
    fn footer_only_advertises_the_basic_controls() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), true).unwrap();
        monitor.remember_live(test_time(0), vec![reading(7.3)]);
        for width in [48, 80, 140] {
            let mut terminal = Terminal::new(TestBackend::new(width, 32)).unwrap();
            terminal
                .draw(|frame| draw(frame, &monitor, &View::default(), Path::new("missing.db")))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let footer = (0..width)
                .map(|x| buffer[(x, 31)].symbol())
                .collect::<String>();
            for needed in ["←→ Window", "↑↓ Graph", "? Help", "q Quit"] {
                assert!(footer.contains(needed));
            }
            for advanced in ["Tab", "Focus", "Expand", "Battery", "Details", "Reload"] {
                assert!(!footer.contains(advanced));
            }
            assert!(footer.ends_with("den"));
        }
    }

    #[test]
    fn history_only_is_visibly_distinct_from_live() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Monitor::new(&dir.path().join("missing.db"), dir.path(), false).unwrap();
        monitor.recorded = Some(Snapshot {
            frame_id: 1,
            time: test_time(0),
            interval_secs: 30,
            batteries: vec![reading(8.0)],
        });
        monitor.selected = Some("BAT0".into());
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| draw(frame, &monitor, &View::default(), Path::new("missing.db")))
            .unwrap();
        assert!(text(&terminal).contains("HISTORY"));
        assert!(!text(&terminal).contains("LIVE 1s"));
    }
}
