use crate::compositor::BuiltinValues;
use crate::config::{
    CommandTrigger, Config, IpcMessage, OverlayConfig, ToastLevel, WidgetKind, WidgetSpec,
};
use crate::ui::{CellStyle, Color, Gauge, Line, Span, Sparkline, ThemePalette, ThemePreset};
use chrono::{DateTime, Local};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const MAX_METRIC_HISTORY: usize = 64;
const MAX_TOAST_STACK: usize = 5;

#[derive(Debug, Clone)]
pub(crate) struct SlotEntry {
    pub value: String,
    pub expires_at: Option<Instant>,
}

#[derive(Debug, Clone)]
pub(crate) struct MetricSeries {
    pub history: VecDeque<u8>,
    pub latest: u8,
    pub label: Option<String>,
    pub expires_at: Option<Instant>,
    pub from_ipc: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ToastEntry {
    pub message: String,
    pub level: ToastLevel,
    pub expires_at: Instant,
}

#[derive(Debug, Clone)]
pub(crate) struct DynamicOverlayEntry {
    pub config: OverlayConfig,
    pub expires_at: Option<Instant>,
}

/// In-memory state store inside `Compositor` holding TTL slots, metric series,
/// toast notification queues, periodic command outputs, and foreground command timers.
pub(crate) struct TelemetryStore {
    pub wall_clock: DateTime<Local>,
    pub builtins: BuiltinValues,
    pub active_fg_cmd: Option<(String, Instant)>,
    pub ipc_slots: BTreeMap<String, SlotEntry>,
    pub metrics: BTreeMap<String, MetricSeries>,
    pub toasts: VecDeque<ToastEntry>,
    pub command_outputs: BTreeMap<String, String>,
    pub command_last_run: BTreeMap<String, Instant>,
    pub dynamic_overlays: BTreeMap<String, DynamicOverlayEntry>,
}

impl TelemetryStore {
    pub fn new() -> Self {
        Self {
            wall_clock: Local::now(),
            builtins: BuiltinValues::default(),
            active_fg_cmd: None,
            ipc_slots: BTreeMap::new(),
            metrics: BTreeMap::new(),
            toasts: VecDeque::new(),
            command_outputs: BTreeMap::new(),
            command_last_run: BTreeMap::new(),
            dynamic_overlays: BTreeMap::new(),
        }
    }

    pub fn on_tick(
        &mut self,
        wall_clock: DateTime<Local>,
        builtins: BuiltinValues,
        config: &Config,
        now: Instant,
    ) -> Vec<(String, String)> {
        let cwd_changed = self.builtins.cwd != builtins.cwd && builtins.cwd.is_some();
        let cmd_finished = self.builtins.fg_process.is_some() && builtins.fg_process.is_none();

        self.wall_clock = wall_clock;
        self.update_fg_telemetry(&builtins.fg_process, now);
        if let Some(cpu) = builtins.cpu_usage {
            self.push_metric_sample("cpu".to_string(), cpu, None, None, false);
        }
        self.builtins = builtins;
        self.evict_expired(now);
        self.collect_due_commands(config, now, cwd_changed, cmd_finished)
    }

    pub fn on_command_output(&mut self, id: String, output: &str) {
        let trimmed = output.lines().next().unwrap_or("").trim().to_string();
        if let Some(metric_key) = id.strip_prefix("__metric__:") {
            let cleaned = trimmed.trim_end_matches('%').trim();
            if let Ok(val) = cleaned.parse::<f64>() {
                let clamped = val.round().clamp(0.0, 100.0) as u8;
                self.push_metric_sample(metric_key.to_string(), clamped, None, None, false);
            }
        } else {
            self.command_outputs.insert(id, trimmed);
        }
    }

    fn push_metric_sample(
        &mut self,
        key: String,
        value: u8,
        label: Option<String>,
        expires_at: Option<Instant>,
        from_ipc: bool,
    ) {
        let clamped = value.min(100);
        let entry = self.metrics.entry(key).or_insert_with(|| MetricSeries {
            history: VecDeque::with_capacity(MAX_METRIC_HISTORY),
            latest: clamped,
            label: None,
            expires_at: None,
            from_ipc,
        });
        if entry.history.len() >= MAX_METRIC_HISTORY {
            entry.history.pop_front();
        }
        entry.history.push_back(clamped);
        entry.latest = clamped;
        if label.is_some() {
            entry.label = label;
        }
        if expires_at.is_some() || from_ipc {
            entry.expires_at = expires_at;
        }
        if from_ipc {
            entry.from_ipc = true;
        }
    }

    pub fn apply_ipc(&mut self, msg: IpcMessage, now: Instant) {
        self.evict_expired(now);
        match msg {
            IpcMessage::SetSlot { key, value, ttl_ms } => {
                let expires_at = ttl_ms.map(|ms| now + Duration::from_millis(ms));
                self.ipc_slots.insert(key, SlotEntry { value, expires_at });
            }
            IpcMessage::ClearSlot { key } => {
                self.ipc_slots.remove(&key);
                self.metrics.remove(&key);
            }
            IpcMessage::PushMetric {
                key,
                value,
                label,
                ttl_ms,
            } => {
                let expires_at = ttl_ms.map(|ms| now + Duration::from_millis(ms));
                self.push_metric_sample(key, value, label, expires_at, true);
            }
            IpcMessage::Toast {
                message,
                level,
                ttl_ms,
            } => {
                if self.toasts.len() >= MAX_TOAST_STACK {
                    self.toasts.pop_front();
                }
                self.toasts.push_back(ToastEntry {
                    message,
                    level,
                    expires_at: now + Duration::from_millis(ttl_ms),
                });
            }
            IpcMessage::UpsertOverlay { overlay } => {
                let expires_at = overlay.ttl_ms.map(|ms| now + Duration::from_millis(ms));
                self.dynamic_overlays.insert(
                    overlay.id.clone(),
                    DynamicOverlayEntry {
                        config: overlay,
                        expires_at,
                    },
                );
            }
            IpcMessage::RemoveOverlay { id } => {
                self.dynamic_overlays.remove(&id);
            }
            IpcMessage::Quit => {}
        }
    }

    pub fn collect_due_commands(
        &mut self,
        config: &Config,
        now: Instant,
        cwd_changed: bool,
        cmd_finished: bool,
    ) -> Vec<(String, String)> {
        let mut specs = Vec::new();
        for w in &config.status_bar.left {
            specs.push(&w.kind);
        }
        for w in &config.status_bar.center {
            specs.push(&w.kind);
        }
        for w in &config.status_bar.right {
            specs.push(&w.kind);
        }
        for ov in &config.overlays {
            for w in &ov.widgets {
                specs.push(&w.kind);
            }
        }
        for ov in self.dynamic_overlays.values() {
            for w in &ov.config.widgets {
                specs.push(&w.kind);
            }
        }

        let current_cwd = self.builtins.cwd.as_deref();
        let mut due = Vec::new();
        for kind in specs {
            match kind {
                WidgetKind::Command {
                    id,
                    cmd,
                    trigger,
                    interval_ms,
                    when_files,
                } => {
                    if !matches_when_files(current_cwd, when_files) {
                        self.command_outputs.remove(id);
                        self.command_last_run.remove(id);
                        continue;
                    }

                    let effective_trigger = trigger.unwrap_or(if interval_ms.is_some() {
                        CommandTrigger::Interval
                    } else {
                        CommandTrigger::Change
                    });

                    let is_due = match effective_trigger {
                        CommandTrigger::Interval => {
                            let interval = Duration::from_millis(interval_ms.unwrap_or(5000));
                            match self.command_last_run.get(id) {
                                None => true,
                                Some(&last) => now.saturating_duration_since(last) >= interval,
                            }
                        }
                        CommandTrigger::Cwd => {
                            !self.command_last_run.contains_key(id) || cwd_changed
                        }
                        CommandTrigger::Change => {
                            !self.command_last_run.contains_key(id) || cwd_changed || cmd_finished
                        }
                    };

                    if is_due {
                        self.command_last_run.insert(id.clone(), now);
                        due.push((id.clone(), cmd.clone()));
                    }
                }
                WidgetKind::Sparkline {
                    key,
                    cmd: Some(cmd),
                    interval_ms,
                    ..
                }
                | WidgetKind::Gauge {
                    key,
                    cmd: Some(cmd),
                    interval_ms,
                    ..
                } => {
                    let cmd_id = format!("__metric__:{key}");
                    let interval = Duration::from_millis(interval_ms.unwrap_or(2000));
                    let is_due = match self.command_last_run.get(&cmd_id) {
                        None => true,
                        Some(&last) => now.saturating_duration_since(last) >= interval,
                    };
                    if is_due {
                        self.command_last_run.insert(cmd_id.clone(), now);
                        due.push((cmd_id, cmd.clone()));
                    }
                }
                _ => {}
            }
        }
        due
    }

    /// Resolves the `(fg, bg)` accent colors for a widget from `theme` when the user has not
    /// explicitly set `spec.style.fg` or `spec.style.bg`.
    fn resolve_widget_style(
        spec: &WidgetSpec,
        fallback_style: CellStyle,
        theme: ThemePreset,
        in_status_bar: bool,
    ) -> CellStyle {
        let palette: ThemePalette = theme.palette();
        let (default_fg, default_bg) = if in_status_bar && theme != ThemePreset::Default {
            match &spec.kind {
                WidgetKind::Cwd => palette.cwd_accent,
                WidgetKind::GitBranch => palette.git_accent,
                WidgetKind::ActiveCommand { .. } => palette.cmd_accent,
                WidgetKind::Sparkline { .. } | WidgetKind::Gauge { .. } => palette.metric_accent,
                WidgetKind::Battery => palette.battery_accent,
                _ => (fallback_style.fg, fallback_style.bg),
            }
        } else {
            (fallback_style.fg, fallback_style.bg)
        };

        CellStyle {
            fg: spec.style.fg.unwrap_or(default_fg),
            bg: spec.style.bg.unwrap_or(default_bg),
            bold: spec.style.bold.unwrap_or(fallback_style.bold),
            italic: fallback_style.italic,
            underline: fallback_style.underline,
            inverse: fallback_style.inverse,
        }
    }

    /// Evaluates a single `WidgetSpec` into one or more styled `Line`s using `src/ui/` components.
    pub fn evaluate_widget_lines(
        &self,
        spec: &WidgetSpec,
        fallback_style: CellStyle,
        theme: ThemePreset,
        in_status_bar: bool,
        now: Instant,
    ) -> Vec<Line> {
        let seg_style = Self::resolve_widget_style(spec, fallback_style, theme, in_status_bar);
        let outer_bg = fallback_style.bg;
        let icon = spec.style.icon.as_deref();
        let padding = spec.style.padding.unwrap_or(false);
        let pill = spec.style.pill.unwrap_or(false);
        let prefix = spec.style.prefix.as_deref();
        let suffix = spec.style.suffix.as_deref();
        let track_fg = spec.style.track_fg.or_else(|| {
            if theme != ThemePreset::Default {
                Some(theme.palette().track_fg)
            } else {
                None
            }
        });

        match &spec.kind {
            WidgetKind::Sparkline { key, width, .. } => {
                let Some(series) = self.metrics.get(key) else {
                    return Vec::new();
                };
                let line = Sparkline::new(&series.history, *width, seg_style)
                    .to_line()
                    .decorate_segment(icon, padding, pill, prefix, suffix, seg_style, outer_bg);
                if line.is_empty() {
                    Vec::new()
                } else {
                    vec![line]
                }
            }
            WidgetKind::Gauge {
                key, width, label, ..
            } => {
                let Some(series) = self.metrics.get(key) else {
                    return Vec::new();
                };
                let effective_label = series.label.as_deref().or(label.as_deref());
                let line = Gauge::new(series.latest, *width, effective_label, seg_style)
                    .with_track_fg(track_fg)
                    .to_line()
                    .decorate_segment(icon, padding, pill, prefix, suffix, seg_style, outer_bg);
                if line.is_empty() {
                    Vec::new()
                } else {
                    vec![line]
                }
            }
            WidgetKind::ActiveCommand { min_secs } => {
                let Some((name, started)) = &self.active_fg_cmd else {
                    return Vec::new();
                };
                let secs = now.saturating_duration_since(*started).as_secs();
                if secs < *min_secs {
                    return Vec::new();
                }
                let effective_icon = icon.or(Some("⚙"));
                let line = Line::new(vec![Span::styled(format!("{name} ({secs}s)"), seg_style)])
                    .decorate_segment(
                        effective_icon,
                        padding,
                        pill,
                        prefix,
                        suffix,
                        seg_style,
                        outer_bg,
                    );
                vec![line]
            }
            other => {
                let raw = match other {
                    WidgetKind::Clock { format } => self.wall_clock.format(format).to_string(),
                    WidgetKind::Cwd => self.builtins.cwd.clone().unwrap_or_default(),
                    WidgetKind::GitBranch => self.builtins.git_branch.clone().unwrap_or_default(),
                    WidgetKind::Battery => self.builtins.battery.clone().unwrap_or_default(),
                    WidgetKind::Text { text } => text.clone(),
                    WidgetKind::Command { id, .. } => {
                        self.command_outputs.get(id).cloned().unwrap_or_default()
                    }
                    WidgetKind::IpcSlot { key } => self
                        .ipc_slots
                        .get(key)
                        .map(|s| s.value.clone())
                        .unwrap_or_default(),
                    WidgetKind::Sparkline { .. }
                    | WidgetKind::Gauge { .. }
                    | WidgetKind::ActiveCommand { .. } => unreachable!(),
                };

                if raw.is_empty() {
                    return Vec::new();
                }

                raw.lines()
                    .map(|l| {
                        Line::styled(l, seg_style).decorate_segment(
                            icon, padding, pill, prefix, suffix, seg_style, outer_bg,
                        )
                    })
                    .collect()
            }
        }
    }

    /// Concatenates a group of status bar `WidgetSpec`s into a single composite `Line` separated by `sep`.
    pub fn build_segment_line(
        &self,
        specs: &[WidgetSpec],
        sep: &str,
        bar_style: CellStyle,
        theme: ThemePreset,
        now: Instant,
    ) -> Line {
        let mut out = Line::default();
        for spec in specs {
            let Some(first_line) = self
                .evaluate_widget_lines(spec, bar_style, theme, true, now)
                .into_iter()
                .next()
            else {
                continue;
            };
            if first_line.is_empty() {
                continue;
            }
            if !out.is_empty() && !sep.is_empty() {
                out.push_span(Span::styled(sep, bar_style));
            }
            out.spans.extend(first_line.spans);
        }
        out
    }

    pub fn is_metric_referenced(&self, config: &Config, target_key: &str) -> bool {
        let all_widgets = config
            .status_bar
            .left
            .iter()
            .chain(config.status_bar.center.iter())
            .chain(config.status_bar.right.iter())
            .chain(config.overlays.iter().flat_map(|ov| ov.widgets.iter()))
            .chain(
                self.dynamic_overlays
                    .values()
                    .flat_map(|ov| ov.config.widgets.iter()),
            );

        for w in all_widgets {
            match &w.kind {
                WidgetKind::Sparkline { key, .. } | WidgetKind::Gauge { key, .. }
                    if key == target_key =>
                {
                    return true;
                }
                _ => {}
            }
        }
        false
    }

    fn update_fg_telemetry(&mut self, current_fg: &Option<String>, now: Instant) {
        match current_fg {
            Some(proc_name) if !proc_name.is_empty() => match &self.active_fg_cmd {
                Some((existing, _)) if existing == proc_name => {}
                _ => {
                    self.active_fg_cmd = Some((proc_name.clone(), now));
                }
            },
            _ => {
                self.active_fg_cmd = None;
            }
        }
    }

    fn evict_expired(&mut self, now: Instant) {
        self.ipc_slots
            .retain(|_, entry| entry.expires_at.is_none_or(|exp| exp > now));
        self.metrics
            .retain(|_, entry| entry.expires_at.is_none_or(|exp| exp > now));
        self.toasts.retain(|t| t.expires_at > now);
        self.dynamic_overlays
            .retain(|_, entry| entry.expires_at.is_none_or(|exp| exp > now));
    }
}

pub(crate) fn expand_tilde_path(cwd: &str) -> PathBuf {
    if cwd == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home);
        }
    } else if let Some(rest) = cwd.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return Path::new(&home).join(rest);
    }
    PathBuf::from(cwd)
}

fn matches_when_files(cwd: Option<&str>, when_files: &[String]) -> bool {
    if when_files.is_empty() {
        return true;
    }
    let Some(cwd_str) = cwd else {
        return false;
    };
    let start_dir = expand_tilde_path(cwd_str);
    for ancestor in start_dir.ancestors() {
        for file_name in when_files {
            if ancestor.join(file_name).exists() {
                return true;
            }
        }
    }
    false
}

/// Suppress unused import warning when `Color` is used through `CellStyle`
const _: Option<Color> = None;
