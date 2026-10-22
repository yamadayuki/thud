use crate::compositor::effective_widgets;
use crate::compositor::layout::PlacementSolver;
use crate::compositor::scheduler::CommandScheduler;
use crate::compositor::store::TelemetryStore;
use crate::config::{
    Config, OverlayConfig, ScreenCondition, StatusBarPosition, ToastLevel, WidgetKind, WidgetSpec,
};
use crate::ui::{
    Block, Buffer, CellStyle, Color, Gauge, Line, Rect, Span, Sparkline, StatusBar, ThemePalette,
    Toast, Widget,
};
use std::time::Instant;

/// Paints the HUD (status bar, floating overlays, toast stack) over the projected child
/// screen, evaluating each widget from HUD state and resolving theme colors.
pub(crate) struct HudPainter<'a> {
    pub config: &'a Config,
    pub store: &'a TelemetryStore,
    pub commands: &'a CommandScheduler,
    pub now: Instant,
}

impl HudPainter<'_> {
    /// Paints into `buf` around and over `child_rect`. `is_alt` selects overlays by
    /// `screen_mode`.
    pub fn paint(&self, buf: &mut Buffer, child_rect: Rect, is_alt: bool) {
        self.paint_status_bar(buf);

        let mut overlays: Vec<&OverlayConfig> = self.config.overlays.iter().collect();
        overlays.extend(self.store.dynamic_overlays());
        overlays.sort_by_key(|ov| ov.z_index);
        for ov in overlays {
            self.paint_overlay(buf, ov, child_rect, is_alt);
        }

        self.paint_toast_stack(buf, child_rect);
    }

    fn palette(&self) -> ThemePalette {
        self.config.theme.palette()
    }

    fn paint_status_bar(&self, buf: &mut Buffer) {
        let bar = &self.config.status_bar;
        let reserved = self.config.reserved_status_rows();
        if !bar.enabled || reserved == 0 || buf.rows <= reserved {
            return;
        }
        let row = match bar.position {
            StatusBarPosition::Bottom => buf.rows - reserved,
            StatusBarPosition::Top => 0,
        };
        let palette = self.palette();
        let base_style = CellStyle::new(
            bar.fg.unwrap_or(palette.bar_fg),
            bar.bg.unwrap_or(palette.bar_bg),
            false,
        );
        StatusBar {
            base_style,
            left: self.segment_line(&bar.left, base_style),
            center: self.segment_line(&bar.center, base_style),
            right: self.segment_line(&bar.right, base_style),
        }
        .render(Rect::new(0, row, buf.cols, reserved), buf);
    }

    fn paint_overlay(&self, buf: &mut Buffer, ov: &OverlayConfig, child_rect: Rect, is_alt: bool) {
        match ov.screen_mode {
            ScreenCondition::PrimaryOnly if is_alt => return,
            ScreenCondition::AlternateOnly if !is_alt => return,
            _ => {}
        }

        let palette = self.palette();
        let base_style = CellStyle::new(
            ov.fg.unwrap_or(palette.overlay_fg),
            ov.bg.unwrap_or(palette.overlay_bg),
            ov.bold,
        );
        let lines: Vec<Line> = ov
            .widgets
            .iter()
            .flat_map(|w| self.widget_lines(w, base_style, false))
            .collect();
        if lines.is_empty() {
            return;
        }

        let block = Block::new(ov.border, ov.title.as_deref(), base_style, lines)
            .with_border_and_title_fg(
                ov.border_fg.or(Some(palette.border_fg)),
                ov.title_fg.or(Some(palette.title_fg)),
            )
            .with_padding_x(ov.padding_x);
        let (width, height) = block.measure();
        if let Some(rect) = PlacementSolver::resolve_overlay_rect(
            buf,
            child_rect,
            ov.anchor,
            ov.offset_x,
            ov.offset_y,
            width,
            height,
            ov.collision,
        ) {
            block.render(rect, buf);
        }
    }

    /// Paints unreferenced IPC progress gauges (`thud progress`) followed by toasts, newest first.
    fn paint_toast_stack(&self, buf: &mut Buffer, child_rect: Rect) {
        let palette = self.palette();
        let toast_cfg = &self.config.toast;
        let mut items: Vec<(Line, CellStyle)> = Vec::new();

        for (key, series) in self.store.metrics() {
            if !series.from_ipc || self.is_metric_referenced(key) {
                continue;
            }
            let style = CellStyle::new(palette.progress.0, palette.progress.1, true);
            let label = series.label.as_deref().unwrap_or(key);
            let line = Gauge::new(series.latest, 10, Some(label), style)
                .to_line()
                .with_affixes(Some(" "), Some(" "), style);
            items.push((line, style));
        }

        for toast in self.store.toasts().rev() {
            let (fg, bg) = match toast.level {
                ToastLevel::Info => palette.toast_info,
                ToastLevel::Success => palette.toast_success,
                ToastLevel::Warn => palette.toast_warn,
                ToastLevel::Error => palette.toast_error,
            };
            let style = CellStyle::new(fg, bg, true);
            let line = Toast {
                icon: toast.level.icon(),
                message: &toast.message,
                style,
            }
            .to_line();
            items.push((line, style));
        }

        let blocks: Vec<Block> = items
            .into_iter()
            .map(|(mut line, style)| {
                if toast_cfg.pill {
                    line =
                        line.decorate_segment(None, false, true, None, None, style, Color::Default);
                }
                Block::new(toast_cfg.border, None, style, vec![line])
            })
            .collect();
        let sizes: Vec<(u16, u16)> = blocks.iter().map(Block::measure).collect();
        let mut rects = vec![None; blocks.len()];
        for (i, rect) in PlacementSolver::resolve_toast_stack(buf, child_rect, toast_cfg, &sizes) {
            rects[i] = Some(rect);
        }
        for (block, rect) in blocks.into_iter().zip(rects) {
            if let Some(rect) = rect {
                block.render(rect, buf);
            }
        }
    }

    fn is_metric_referenced(&self, target: &str) -> bool {
        effective_widgets(self.config, self.store).any(|w| {
            matches!(&w.kind, WidgetKind::Sparkline { key, .. } | WidgetKind::Gauge { key, .. } if key == target)
        })
    }

    /// Concatenates the first line of each status bar widget, separated by the bar separator.
    fn segment_line(&self, specs: &[WidgetSpec], bar_style: CellStyle) -> Line {
        let sep = &self.config.status_bar.separator;
        let mut out = Line::default();
        for spec in specs {
            let Some(first_line) = self.widget_lines(spec, bar_style, true).into_iter().next()
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

    /// Resolves the `(fg, bg)` accent colors for a widget from the theme when the user has not
    /// explicitly set `spec.style.fg` or `spec.style.bg`.
    fn widget_style(
        &self,
        spec: &WidgetSpec,
        fallback: CellStyle,
        in_status_bar: bool,
    ) -> CellStyle {
        let palette = self.palette();
        let (default_fg, default_bg) = if in_status_bar {
            match &spec.kind {
                WidgetKind::Cwd => palette.cwd_accent,
                WidgetKind::GitBranch => palette.git_accent,
                WidgetKind::ActiveCommand { .. } => palette.cmd_accent,
                WidgetKind::Sparkline { .. } | WidgetKind::Gauge { .. } => palette.metric_accent,
                _ => (fallback.fg, fallback.bg),
            }
        } else {
            (fallback.fg, fallback.bg)
        };

        CellStyle {
            fg: spec.style.fg.unwrap_or(default_fg),
            bg: spec.style.bg.unwrap_or(default_bg),
            bold: spec.style.bold.unwrap_or(fallback.bold),
            italic: fallback.italic,
            underline: fallback.underline,
            inverse: fallback.inverse,
        }
    }

    /// Evaluates a single `WidgetSpec` into one or more styled `Line`s.
    fn widget_lines(
        &self,
        spec: &WidgetSpec,
        fallback: CellStyle,
        in_status_bar: bool,
    ) -> Vec<Line> {
        let seg_style = self.widget_style(spec, fallback, in_status_bar);
        let outer_bg = fallback.bg;
        let icon = spec.style.icon.as_deref();
        let padding = spec.style.padding.unwrap_or(false);
        let pill = spec.style.pill.unwrap_or(false);
        let prefix = spec.style.prefix.as_deref();
        let suffix = spec.style.suffix.as_deref();
        let decorate = |line: Line, icon: Option<&str>| {
            line.decorate_segment(icon, padding, pill, prefix, suffix, seg_style, outer_bg)
        };
        let non_empty = |line: Line| {
            if line.is_empty() {
                Vec::new()
            } else {
                vec![line]
            }
        };

        match &spec.kind {
            WidgetKind::Sparkline {
                key, width, label, ..
            } => {
                let Some(series) = self.store.metric(key) else {
                    return Vec::new();
                };
                let label = series.label.as_deref().or(label.as_deref());
                non_empty(decorate(
                    Sparkline::new(&series.history, *width, label, seg_style).to_line(),
                    icon,
                ))
            }
            WidgetKind::Gauge {
                key, width, label, ..
            } => {
                let Some(series) = self.store.metric(key) else {
                    return Vec::new();
                };
                let label = series.label.as_deref().or(label.as_deref());
                let track_fg = spec.style.track_fg.or(Some(self.palette().track_fg));
                non_empty(decorate(
                    Gauge::new(series.latest, *width, label, seg_style)
                        .with_track_fg(track_fg)
                        .to_line(),
                    icon,
                ))
            }
            WidgetKind::ActiveCommand { min_secs } => {
                let Some((name, started)) = self.store.active_command() else {
                    return Vec::new();
                };
                let secs = self.now.saturating_duration_since(started).as_secs();
                if secs < *min_secs {
                    return Vec::new();
                }
                vec![decorate(
                    Line::new(vec![Span::styled(format!("{name} ({secs}s)"), seg_style)]),
                    icon.or(Some("⚙")),
                )]
            }
            other => {
                let builtins = self.store.builtins();
                let raw = match other {
                    WidgetKind::Clock { format } => {
                        self.store.wall_clock().format(format).to_string()
                    }
                    WidgetKind::Cwd => builtins.cwd.clone().unwrap_or_default(),
                    WidgetKind::GitBranch => builtins.git_branch.clone().unwrap_or_default(),
                    WidgetKind::Text { text } => text.clone(),
                    WidgetKind::Command { id, .. } => {
                        self.commands.output(id).unwrap_or_default().to_string()
                    }
                    WidgetKind::IpcSlot { key } => {
                        self.store.slot(key).unwrap_or_default().to_string()
                    }
                    WidgetKind::Sparkline { .. }
                    | WidgetKind::Gauge { .. }
                    | WidgetKind::ActiveCommand { .. } => unreachable!(),
                };
                raw.lines()
                    .map(|l| decorate(Line::styled(l, seg_style), icon))
                    .collect()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::IpcMessage;
    use crate::ui::{Color, ThemeConfig, ThemePreset};

    fn paint_toast(config: &Config, level: ToastLevel) -> Buffer {
        let now = Instant::now();
        let mut store = TelemetryStore::new();
        store.apply_ipc(
            IpcMessage::Toast {
                message: "done".to_string(),
                level,
                ttl_ms: 1000,
            },
            now,
        );
        let mut buf = Buffer::new(30, 6);
        HudPainter {
            config,
            store: &store,
            commands: &CommandScheduler::default(),
            now,
        }
        .paint(&mut buf, Rect::new(0, 0, 30, 5), false);
        buf
    }

    fn style_of(buf: &Buffer, needle: char) -> CellStyle {
        buf.cells
            .iter()
            .find(|c| c.ch == needle)
            .expect("toast text painted")
            .style
    }

    fn bare_config(theme: ThemeConfig) -> Config {
        let mut config = Config::default();
        config.overlays.clear();
        config.status_bar.enabled = false;
        config.theme = theme;
        config
    }

    #[test]
    fn toast_colors_follow_the_theme_palette() {
        let default = paint_toast(&bare_config(ThemeConfig::default()), ToastLevel::Info);
        assert_eq!(style_of(&default, 'd').bg, Color::Ansi(24));

        let tokyo = paint_toast(
            &bare_config(ThemePreset::TokyoNight.into()),
            ToastLevel::Error,
        );
        assert_eq!(style_of(&tokyo, 'd').bg, Color::Rgb(247, 118, 142));

        let mut overridden = ThemeConfig::from(ThemePreset::Nord);
        overridden.toast_success_bg = Some(Color::Rgb(1, 2, 3));
        let custom = paint_toast(&bare_config(overridden), ToastLevel::Success);
        assert_eq!(style_of(&custom, 'd').bg, Color::Rgb(1, 2, 3));
    }

    #[test]
    fn theme_table_accepts_toast_and_progress_overrides() {
        let config = Config::from_toml(
            r##"
            theme = { preset = "gruvbox", toast_warn_fg = "#000000", progress_bg = "blue" }
            "##,
        )
        .unwrap();
        let palette = config.theme.palette();
        assert_eq!(palette.toast_warn.0, Color::Rgb(0, 0, 0));
        assert_eq!(palette.toast_warn.1, Color::Rgb(250, 189, 47));
        assert_ne!(
            palette.progress.1,
            ThemePreset::Gruvbox.palette().progress.1
        );
    }
}
