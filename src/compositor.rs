use crate::config::{
    Anchor, CellStyle, CollisionPolicy, Color, Config, ConfigError, IpcMessage, OverlayConfig,
    ScreenCondition, TermSize, WidgetSpec,
};
use chrono::{DateTime, Local};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthChar;

/// Built-in environment values passed into `Event::Tick`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuiltinValues {
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub battery: Option<String>,
}

/// All external occurrences that drive the Sans-I/O `Compositor` state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event<'a> {
    /// Raw output bytes read from the child PTY master fd.
    PtyOutput(&'a [u8]),
    /// Host terminal window resize (`SIGWINCH`).
    HostResize(TermSize),
    /// Periodic timer tick with current local wall clock and builtin environment snapshot.
    Tick {
        wall_clock: DateTime<Local>,
        builtins: BuiltinValues,
    },
    /// Completed output from an asynchronous periodic shell command (`sh -c ...`).
    CommandOutput { id: String, output: String },
    /// Dynamic update received from the Unix domain socket IPC server.
    Ipc(IpcMessage),
}

/// Deterministic output effects produced by a single `Compositor` transition.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Transition {
    /// Minimal ANSI escape sequence diff to write to the host terminal's stdout.
    pub host_ansi_diff: Vec<u8>,
    /// New child PTY dimensions to apply via `TIOCSWINSZ` if changed.
    pub child_pty_resize: Option<TermSize>,
    /// Periodic shell commands `(id, cmd)` whose schedule is now due for async execution.
    pub due_commands: Vec<(String, String)>,
}

/// A single cell in the composited terminal frame grid.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RenderCell {
    /// Text content of this cell (empty string if this cell is the 2nd column of a wide char).
    ch: String,
    /// Display width of this cell: `1` for normal, `2` for wide head cell, `0` for wide continuation cell.
    width: u8,
    style: CellStyle,
}

impl Default for RenderCell {
    fn default() -> Self {
        Self {
            ch: " ".to_string(),
            width: 1,
            style: CellStyle::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrameGrid {
    cols: u16,
    rows: u16,
    cells: Vec<RenderCell>,
    cursor_row: u16,
    cursor_col: u16,
    hide_cursor: bool,
}

impl FrameGrid {
    fn new(cols: u16, rows: u16) -> Self {
        let len = usize::from(cols) * usize::from(rows);
        Self {
            cols,
            rows,
            cells: vec![RenderCell::default(); len],
            cursor_row: 0,
            cursor_col: 0,
            hide_cursor: false,
        }
    }

    fn idx(&self, row: u16, col: u16) -> Option<usize> {
        if row < self.rows && col < self.cols {
            Some(usize::from(row) * usize::from(self.cols) + usize::from(col))
        } else {
            None
        }
    }

    fn get(&self, row: u16, col: u16) -> Option<&RenderCell> {
        self.idx(row, col).map(|i| &self.cells[i])
    }

    /// Writes a single character at `(row, col)` while healing any split 2-cell wide characters
    /// at the left or right boundary of the overwrite.
    fn paint_char(&mut self, row: u16, col: u16, c: char, char_width: u8, style: CellStyle) {
        if row >= self.rows || col >= self.cols {
            return;
        }
        if char_width == 2 && col + 1 >= self.cols {
            // Cannot fit a 2-column character at the very last column; pad with space instead.
            self.paint_char(row, col, ' ', 1, style);
            return;
        }

        // 1. Heal left neighbor if we are overwriting the continuation cell (2nd half) of a wide char.
        if let Some(cur_idx) = self.idx(row, col)
            && self.cells[cur_idx].width == 0
            && col > 0
            && let Some(left_idx) = self.idx(row, col - 1)
        {
            let bg_style = self.cells[left_idx].style;
            self.cells[left_idx] = RenderCell {
                ch: " ".to_string(),
                width: 1,
                style: bg_style,
            };
        }

        // 2. Heal right neighbor if we are overwriting the head cell (1st half) of a wide char
        // with a 1-column character.
        if char_width == 1
            && let Some(cur_idx) = self.idx(row, col)
            && self.cells[cur_idx].width == 2
            && let Some(right_idx) = self.idx(row, col + 1)
        {
            let bg_style = self.cells[right_idx].style;
            self.cells[right_idx] = RenderCell {
                ch: " ".to_string(),
                width: 1,
                style: bg_style,
            };
        }

        // 3. If writing a 2-column character, also check what resides at `col + 1`.
        if char_width == 2
            && let Some(next_idx) = self.idx(row, col + 1)
            && self.cells[next_idx].width == 2
            && let Some(after_idx) = self.idx(row, col + 2)
        {
            let bg_style = self.cells[after_idx].style;
            self.cells[after_idx] = RenderCell {
                ch: " ".to_string(),
                width: 1,
                style: bg_style,
            };
        }

        // Write head cell
        if let Some(idx) = self.idx(row, col) {
            self.cells[idx] = RenderCell {
                ch: c.to_string(),
                width: char_width,
                style,
            };
        }

        // Write continuation cell if width == 2
        if char_width == 2
            && let Some(next_idx) = self.idx(row, col + 1)
        {
            self.cells[next_idx] = RenderCell {
                ch: String::new(),
                width: 0,
                style,
            };
        }
    }

    /// Paints a string starting at `(row, start_col)`, clipped to `max_cols`.
    fn paint_str(&mut self, row: u16, start_col: u16, text: &str, style: CellStyle) -> u16 {
        let mut col = start_col;
        for c in text.chars() {
            if c == '\n' || c == '\r' {
                continue;
            }
            let w = UnicodeWidthChar::width(c).unwrap_or(0);
            if w == 0 {
                continue;
            }
            let w_u8 = w as u8;
            if col + u16::from(w_u8) > self.cols {
                break;
            }
            self.paint_char(row, col, c, w_u8, style);
            col += u16::from(w_u8);
        }
        col.saturating_sub(start_col)
    }

    fn to_plain_text(&self) -> String {
        let mut out = String::new();
        for r in 0..self.rows {
            if r > 0 {
                out.push('\n');
            }
            for c in 0..self.cols {
                if let Some(cell) = self.get(r, c)
                    && cell.width > 0
                {
                    out.push_str(&cell.ch);
                }
            }
        }
        out
    }
}

#[derive(Debug, Clone)]
struct SlotEntry {
    value: String,
    expires_at: Option<Instant>,
}

#[derive(Debug, Clone)]
struct DynamicOverlayEntry {
    config: OverlayConfig,
    expires_at: Option<Instant>,
}

/// Sans-I/O Deep Module that owns virtual terminal emulation, docked status-line geometry,
/// floating overlay layout, cursor collision dodging, CJK wide-character healing, and minimal
/// ANSI diff generation.
pub struct Compositor {
    host_size: TermSize,
    config: Config,
    vt: vt100::Parser,
    wall_clock: DateTime<Local>,
    builtins: BuiltinValues,
    ipc_slots: BTreeMap<String, SlotEntry>,
    command_outputs: BTreeMap<String, String>,
    command_last_run: BTreeMap<String, Instant>,
    dynamic_overlays: BTreeMap<String, DynamicOverlayEntry>,
    prev_frame: Option<FrameGrid>,
}

impl Compositor {
    /// Entry point 1: Initializes the `Compositor` and returns the initial `Transition`
    /// (initial child PTY dimensions, initial frame render, and initially due commands).
    pub fn new(
        initial_host_size: TermSize,
        config: Config,
        now: Instant,
    ) -> Result<(Self, Transition), ConfigError> {
        if initial_host_size.cols == 0 || initial_host_size.rows == 0 {
            return Err(ConfigError::InvalidGeometry(
                "host terminal dimensions must be at least 1x1".to_string(),
            ));
        }

        let child_size = Self::compute_child_size(initial_host_size, &config);
        let vt = vt100::Parser::new(child_size.rows, child_size.cols, 0);

        let mut comp = Self {
            host_size: initial_host_size,
            config,
            vt,
            wall_clock: Local::now(),
            builtins: BuiltinValues::default(),
            ipc_slots: BTreeMap::new(),
            command_outputs: BTreeMap::new(),
            command_last_run: BTreeMap::new(),
            dynamic_overlays: BTreeMap::new(),
            prev_frame: None,
        };

        let due_commands = comp.collect_due_commands(now);
        let host_ansi_diff = comp.compose_and_diff();

        Ok((
            comp,
            Transition {
                host_ansi_diff,
                child_pty_resize: Some(child_size),
                due_commands,
            },
        ))
    }

    /// Entry point 2: Processes a single `Event` deterministically and returns the resulting `Transition`.
    /// This method is infallible.
    pub fn step(&mut self, event: Event<'_>, now: Instant) -> Transition {
        let mut child_pty_resize = None;
        let mut due_commands = Vec::new();

        match event {
            Event::PtyOutput(bytes) => {
                self.vt.process(bytes);
            }
            Event::HostResize(new_size) => {
                let clamped = TermSize {
                    cols: new_size.cols.max(1),
                    rows: new_size.rows.max(1),
                };
                if clamped != self.host_size {
                    self.host_size = clamped;
                    let new_child_size = Self::compute_child_size(self.host_size, &self.config);
                    self.vt.set_size(new_child_size.rows, new_child_size.cols);
                    // Force a full redraw after host terminal resize
                    self.prev_frame = None;
                    child_pty_resize = Some(new_child_size);
                }
            }
            Event::Tick {
                wall_clock,
                builtins,
            } => {
                self.wall_clock = wall_clock;
                self.builtins = builtins;
                self.evict_expired(now);
                due_commands = self.collect_due_commands(now);
            }
            Event::CommandOutput { id, output } => {
                let trimmed = output.lines().next().unwrap_or("").trim().to_string();
                self.command_outputs.insert(id, trimmed);
            }
            Event::Ipc(msg) => {
                self.evict_expired(now);
                match msg {
                    IpcMessage::SetSlot { key, value, ttl_ms } => {
                        let expires_at = ttl_ms.map(|ms| now + Duration::from_millis(ms));
                        self.ipc_slots.insert(key, SlotEntry { value, expires_at });
                    }
                    IpcMessage::ClearSlot { key } => {
                        self.ipc_slots.remove(&key);
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
                }
            }
        }

        let host_ansi_diff = self.compose_and_diff();
        Transition {
            host_ansi_diff,
            child_pty_resize,
            due_commands,
        }
    }

    /// Returns the current composited screen as plain text (one line per row, separated by `\n`).
    pub fn rendered_text(&self) -> String {
        self.build_frame_grid().to_plain_text()
    }

    /// Returns the child PTY dimensions calculated for the current host terminal size.
    pub fn child_size(&self) -> TermSize {
        Self::compute_child_size(self.host_size, &self.config)
    }

    fn compute_child_size(host_size: TermSize, config: &Config) -> TermSize {
        let reserved = config.reserved_bottom_rows();
        let child_rows = if host_size.rows > reserved {
            host_size.rows - reserved
        } else {
            1
        };
        TermSize {
            cols: host_size.cols.max(1),
            rows: child_rows.max(1),
        }
    }

    fn evict_expired(&mut self, now: Instant) {
        self.ipc_slots
            .retain(|_, entry| entry.expires_at.is_none_or(|exp| exp > now));
        self.dynamic_overlays
            .retain(|_, entry| entry.expires_at.is_none_or(|exp| exp > now));
    }

    fn collect_due_commands(&mut self, now: Instant) -> Vec<(String, String)> {
        let mut specs = Vec::new();
        for w in &self.config.status_bar.left {
            specs.push(w);
        }
        for w in &self.config.status_bar.center {
            specs.push(w);
        }
        for w in &self.config.status_bar.right {
            specs.push(w);
        }
        for ov in &self.config.overlays {
            specs.push(&ov.widget);
        }
        for ov in self.dynamic_overlays.values() {
            specs.push(&ov.config.widget);
        }

        let mut due = Vec::new();
        for spec in specs {
            if let WidgetSpec::Command {
                id,
                cmd,
                interval_ms,
            } = spec
            {
                let interval = Duration::from_millis(*interval_ms);
                let is_due = match self.command_last_run.get(id) {
                    None => true,
                    Some(&last) => now.saturating_duration_since(last) >= interval,
                };
                if is_due {
                    self.command_last_run.insert(id.clone(), now);
                    due.push((id.clone(), cmd.clone()));
                }
            }
        }
        due
    }

    fn evaluate_widget(&self, spec: &WidgetSpec) -> String {
        match spec {
            WidgetSpec::Clock { format } => self.wall_clock.format(format).to_string(),
            WidgetSpec::Cwd => self.builtins.cwd.clone().unwrap_or_default(),
            WidgetSpec::GitBranch => self.builtins.git_branch.clone().unwrap_or_default(),
            WidgetSpec::Battery => self.builtins.battery.clone().unwrap_or_default(),
            WidgetSpec::Text { text } => text.clone(),
            WidgetSpec::Command { id, .. } => {
                self.command_outputs.get(id).cloned().unwrap_or_default()
            }
            WidgetSpec::IpcSlot { key } => self
                .ipc_slots
                .get(key)
                .map(|s| s.value.clone())
                .unwrap_or_default(),
        }
    }

    fn build_frame_grid(&self) -> FrameGrid {
        let mut grid = FrameGrid::new(self.host_size.cols, self.host_size.rows);
        let screen = self.vt.screen();
        let child_size = self.child_size();

        // 1. Copy child virtual terminal buffer into top `child_size.rows` rows
        for r in 0..child_size.rows {
            let mut c = 0;
            while c < child_size.cols {
                if let Some(cell) = screen.cell(r, c) {
                    let style = Self::convert_vt_style(cell);
                    if cell.is_wide_continuation() {
                        c += 1;
                        continue;
                    }
                    let contents = cell.contents();
                    let ch = contents.chars().next().unwrap_or(' ');
                    let width = if cell.is_wide() { 2 } else { 1 };
                    grid.paint_char(r, c, ch, width, style);
                    c += u16::from(width);
                } else {
                    c += 1;
                }
            }
        }

        let (cur_row, cur_col) = screen.cursor_position();
        grid.cursor_row = cur_row.min(child_size.rows.saturating_sub(1));
        grid.cursor_col = cur_col.min(child_size.cols.saturating_sub(1));
        grid.hide_cursor = screen.hide_cursor();

        // 2. Render docked bottom status bar if enabled and terminal is tall enough
        let reserved = self.config.reserved_bottom_rows();
        if self.config.status_bar.enabled && self.host_size.rows > reserved && reserved > 0 {
            let status_row = self.host_size.rows - reserved;
            self.render_status_bar(&mut grid, status_row, reserved);
        }

        // 3. Render floating overlays sorted by z_index
        let mut overlays: Vec<&OverlayConfig> = self.config.overlays.iter().collect();
        for dyn_ov in self.dynamic_overlays.values() {
            overlays.push(&dyn_ov.config);
        }
        overlays.sort_by_key(|ov| ov.z_index);

        let is_alt = screen.alternate_screen();
        for ov in overlays {
            self.render_overlay(&mut grid, ov, child_size, is_alt);
        }

        grid
    }

    fn render_status_bar(&self, grid: &mut FrameGrid, start_row: u16, height: u16) {
        let bar_style = CellStyle {
            fg: Color::Ansi(self.config.status_bar.fg_ansi),
            bg: Color::Ansi(self.config.status_bar.bg_ansi),
            bold: false,
            italic: false,
            underline: false,
            inverse: false,
        };

        // Fill background of all reserved status rows
        for r in start_row..(start_row + height).min(grid.rows) {
            for c in 0..grid.cols {
                grid.paint_char(r, c, ' ', 1, bar_style);
            }
        }

        let sep = &self.config.status_bar.separator;
        let left_str = self.join_widgets(&self.config.status_bar.left, sep);
        let center_str = self.join_widgets(&self.config.status_bar.center, sep);
        let right_str = self.join_widgets(&self.config.status_bar.right, sep);

        // Paint left section
        if !left_str.is_empty() {
            grid.paint_str(start_row, 0, &left_str, bar_style);
        }

        // Paint center section
        if !center_str.is_empty() {
            let center_width = Self::display_width(&center_str);
            let start_col = grid.cols.saturating_sub(center_width) / 2;
            grid.paint_str(start_row, start_col, &center_str, bar_style);
        }

        // Paint right section
        if !right_str.is_empty() {
            let right_width = Self::display_width(&right_str);
            let start_col = grid.cols.saturating_sub(right_width);
            grid.paint_str(start_row, start_col, &right_str, bar_style);
        }
    }

    fn render_overlay(
        &self,
        grid: &mut FrameGrid,
        ov: &OverlayConfig,
        child_size: TermSize,
        is_alt: bool,
    ) {
        match ov.screen_mode {
            ScreenCondition::Always => {}
            ScreenCondition::PrimaryOnly if is_alt => return,
            ScreenCondition::AlternateOnly if !is_alt => return,
            _ => {}
        }

        let text = self.evaluate_widget(&ov.widget);
        if text.is_empty() {
            return;
        }
        let width = Self::display_width(&text);
        if width == 0 || width > child_size.cols {
            return;
        }

        let (mut row, mut col) = Self::resolve_anchor(
            ov.anchor,
            width,
            child_size.cols,
            child_size.rows,
            ov.offset_x,
            ov.offset_y,
        );

        // Check cursor collision policy
        match ov.collision {
            CollisionPolicy::Ignore => {}
            CollisionPolicy::AutoHide { margin } => {
                if Self::cursor_collides(
                    grid.cursor_row,
                    grid.cursor_col,
                    row,
                    col,
                    width,
                    margin,
                ) {
                    return;
                }
            }
            CollisionPolicy::DodgeToOppositeCorner { margin } => {
                if Self::cursor_collides(
                    grid.cursor_row,
                    grid.cursor_col,
                    row,
                    col,
                    width,
                    margin,
                ) {
                    let opposite = ov.anchor.opposite_horizontal();
                    let (dodged_row, dodged_col) = Self::resolve_anchor(
                        opposite,
                        width,
                        child_size.cols,
                        child_size.rows,
                        -ov.offset_x,
                        ov.offset_y,
                    );
                    row = dodged_row;
                    col = dodged_col;
                }
            }
        }

        let style = CellStyle {
            fg: Color::Ansi(ov.fg_ansi),
            bg: Color::Ansi(ov.bg_ansi),
            bold: ov.bold,
            italic: false,
            underline: false,
            inverse: false,
        };

        grid.paint_str(row, col, &text, style);
    }

    fn resolve_anchor(
        anchor: Anchor,
        width: u16,
        cols: u16,
        rows: u16,
        offset_x: i16,
        offset_y: i16,
    ) -> (u16, u16) {
        let base_row: i32 = match anchor {
            Anchor::TopLeft | Anchor::TopCenter | Anchor::TopRight => 0,
            Anchor::BottomLeft | Anchor::BottomCenter | Anchor::BottomRight => {
                i32::from(rows.saturating_sub(1))
            }
        };
        let base_col: i32 = match anchor {
            Anchor::TopLeft | Anchor::BottomLeft => 0,
            Anchor::TopCenter | Anchor::BottomCenter => {
                i32::from(cols.saturating_sub(width) / 2)
            }
            Anchor::TopRight | Anchor::BottomRight => i32::from(cols.saturating_sub(width)),
        };

        let r = (base_row + i32::from(offset_y)).clamp(0, i32::from(rows.saturating_sub(1))) as u16;
        let c = (base_col + i32::from(offset_x))
            .clamp(0, i32::from(cols.saturating_sub(width))) as u16;
        (r, c)
    }

    fn cursor_collides(
        cur_row: u16,
        cur_col: u16,
        ov_row: u16,
        ov_col: u16,
        ov_width: u16,
        margin: u16,
    ) -> bool {
        let min_r = ov_row.saturating_sub(margin);
        let max_r = ov_row.saturating_add(margin);
        let min_c = ov_col.saturating_sub(margin);
        let max_c = ov_col.saturating_add(ov_width).saturating_add(margin);

        cur_row >= min_r && cur_row <= max_r && cur_col >= min_c && cur_col < max_c
    }

    fn join_widgets(&self, specs: &[WidgetSpec], sep: &str) -> String {
        let parts: Vec<String> = specs
            .iter()
            .map(|s| self.evaluate_widget(s))
            .filter(|s| !s.is_empty())
            .collect();
        parts.join(sep)
    }

    fn display_width(text: &str) -> u16 {
        text.chars()
            .filter_map(UnicodeWidthChar::width)
            .sum::<usize>() as u16
    }

    fn convert_vt_style(cell: &vt100::Cell) -> CellStyle {
        CellStyle {
            fg: Self::convert_vt_color(cell.fgcolor()),
            bg: Self::convert_vt_color(cell.bgcolor()),
            bold: cell.bold(),
            italic: cell.italic(),
            underline: cell.underline(),
            inverse: cell.inverse(),
        }
    }

    fn convert_vt_color(c: vt100::Color) -> Color {
        match c {
            vt100::Color::Default => Color::Default,
            vt100::Color::Idx(i) => Color::Ansi(i),
            vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
        }
    }

    /// Computes minimal ANSI escape sequences between `self.prev_frame` and the new `FrameGrid`.
    fn compose_and_diff(&mut self) -> Vec<u8> {
        let next = self.build_frame_grid();
        let mut ansi = String::new();

        let mut any_cell_changed = false;
        let mut current_style: Option<CellStyle> = None;

        for r in 0..next.rows {
            let mut c = 0;
            while c < next.cols {
                let next_cell = &next.cells[usize::from(r) * usize::from(next.cols) + usize::from(c)];
                if next_cell.width == 0 {
                    c += 1;
                    continue;
                }

                let is_same = if let Some(prev) = &self.prev_frame {
                    if prev.cols == next.cols && prev.rows == next.rows {
                        let idx = usize::from(r) * usize::from(next.cols) + usize::from(c);
                        let same_head = prev.cells[idx] == *next_cell;
                        let same_cont = if next_cell.width == 2 && c + 1 < next.cols {
                            prev.cells[idx + 1] == next.cells[idx + 1]
                        } else {
                            true
                        };
                        same_head && same_cont
                    } else {
                        false
                    }
                } else {
                    false
                };

                if !is_same {
                    if !any_cell_changed {
                        // Hide cursor during frame paint to prevent flicker
                        ansi.push_str("\x1b[?25l");
                        any_cell_changed = true;
                    }
                    let _ = write!(ansi, "\x1b[{};{}H", r + 1, c + 1);
                    if current_style != Some(next_cell.style) {
                        Self::write_sgr(&mut ansi, next_cell.style);
                        current_style = Some(next_cell.style);
                    }
                    ansi.push_str(&next_cell.ch);
                }

                c += u16::from(next_cell.width.max(1));
            }
        }

        let cursor_changed = self.prev_frame.as_ref().is_none_or(|prev| {
            prev.cursor_row != next.cursor_row
                || prev.cursor_col != next.cursor_col
                || prev.hide_cursor != next.hide_cursor
        });

        if any_cell_changed || cursor_changed {
            // Reset SGR attributes before restoring cursor state
            ansi.push_str("\x1b[0m");
            let _ = write!(
                ansi,
                "\x1b[{};{}H",
                next.cursor_row + 1,
                next.cursor_col + 1
            );
            if next.hide_cursor {
                ansi.push_str("\x1b[?25l");
            } else {
                ansi.push_str("\x1b[?25h");
            }
        }

        self.prev_frame = Some(next);
        ansi.into_bytes()
    }

    fn write_sgr(out: &mut String, style: CellStyle) {
        out.push_str("\x1b[0");
        if style.bold {
            out.push_str(";1");
        }
        if style.italic {
            out.push_str(";3");
        }
        if style.underline {
            out.push_str(";4");
        }
        if style.inverse {
            out.push_str(";7");
        }
        match style.fg {
            Color::Default => {}
            Color::Ansi(idx) => {
                let _ = write!(out, ";38;5;{idx}");
            }
            Color::Rgb(r, g, b) => {
                let _ = write!(out, ";38;2;{r};{g};{b}");
            }
        }
        match style.bg {
            Color::Default => {}
            Color::Ansi(idx) => {
                let _ = write!(out, ";48;5;{idx}");
            }
            Color::Rgb(r, g, b) => {
                let _ = write!(out, ";48;2;{r};{g};{b}");
            }
        }
        out.push('m');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample_config() -> Config {
        Config::from_toml(
            r#"
            [status_bar]
            enabled = true
            height = 1
            separator = " | "
            left = [
                { type = "cwd" },
                { type = "ipc_slot", key = "mode" }
            ]
            right = [
                { type = "command", id = "custom_cmd", cmd = "echo ok", interval_ms = 2000 }
            ]

            [[overlays]]
            id = "clock"
            anchor = "top_right"
            collision = { mode = "dodge_to_opposite_corner", margin = 1 }
            widget = { type = "clock", format = "%H:%M" }
        "#,
        )
        .unwrap()
    }

    #[test]
    fn reserves_bottom_status_row_and_composes_top_right_clock() {
        let now = Instant::now();
        let (mut comp, init_tx) =
            Compositor::new(TermSize { cols: 20, rows: 4 }, sample_config(), now).unwrap();

        // Child PTY gets 20x3 (1 row reserved for bottom status bar)
        assert_eq!(
            init_tx.child_pty_resize,
            Some(TermSize { cols: 20, rows: 3 })
        );
        // Periodic command is due immediately on startup
        assert_eq!(
            init_tx.due_commands,
            vec![("custom_cmd".to_string(), "echo ok".to_string())]
        );

        let fixed_time = Local.with_ymd_and_hms(2026, 10, 22, 14, 30, 0).unwrap();
        comp.step(
            Event::Tick {
                wall_clock: fixed_time,
                builtins: BuiltinValues {
                    cwd: Some("~/cth".to_string()),
                    git_branch: None,
                    battery: None,
                },
            },
            now,
        );
        comp.step(Event::PtyOutput(b"hello world"), now);
        comp.step(
            Event::Ipc(IpcMessage::SetSlot {
                key: "mode".to_string(),
                value: "NORM".to_string(),
                ttl_ms: None,
            }),
            now,
        );
        comp.step(
            Event::CommandOutput {
                id: "custom_cmd".to_string(),
                output: "R1".to_string(),
            },
            now,
        );

        assert_eq!(
            comp.rendered_text(),
            concat!(
                "hello world    14:30\n",
                "                    \n",
                "                    \n",
                "~/cth | NORM      R1"
            )
        );
    }

    #[test]
    fn heals_split_cjk_wide_characters_and_restores_cleanly_in_alternate_screen() {
        let now = Instant::now();
        let (mut comp, _) =
            Compositor::new(TermSize { cols: 20, rows: 4 }, sample_config(), now).unwrap();

        let fixed_time = Local.with_ymd_and_hms(2026, 10, 22, 9, 5, 0).unwrap();
        comp.step(
            Event::Tick {
                wall_clock: fixed_time,
                builtins: BuiltinValues::default(),
            },
            now,
        );

        // Enter Alternate Screen (?1049h) like Vim and draw "あいうえお" at row 1, col 11..=20
        // Each CJK char is 2 columns wide:
        // col 10..12: あ, 12..14: い, 14..16: う, 16..18: え, 18..20: お
        // Move cursor back to (1, 1) so it does not trigger cursor dodge.
        // Clock "09:05" occupies 5 columns at the right edge: col 15..20.
        // Notice col 15 lands on the 2nd half of 'う' (col 14..16)!
        // Split-wide-char healing must replace col 14 (1st half of 'う') with a space.
        comp.step(
            Event::PtyOutput(b"\x1b[?1049h\x1b[1;11H\xe3\x81\x82\xe3\x81\x84\xe3\x81\x86\xe3\x81\x88\xe3\x81\x8a\x1b[1;1H"),
            now,
        );

        assert_eq!(
            comp.rendered_text().lines().next().unwrap(),
            "          あい 09:05"
        );

        // When the overlay is removed via IPC, the underlying CJK characters "あいうえお" are
        // restored without re-querying the child PTY!
        comp.config.overlays.clear();
        comp.step(Event::PtyOutput(b""), now);
        assert_eq!(
            comp.rendered_text().lines().next().unwrap(),
            "          あいうえお"
        );
    }

    #[test]
    fn dodges_top_right_overlay_to_top_left_when_cursor_approaches() {
        let now = Instant::now();
        let (mut comp, _) =
            Compositor::new(TermSize { cols: 20, rows: 4 }, sample_config(), now).unwrap();

        let fixed_time = Local.with_ymd_and_hms(2026, 10, 22, 14, 30, 0).unwrap();
        comp.step(
            Event::Tick {
                wall_clock: fixed_time,
                builtins: BuiltinValues::default(),
            },
            now,
        );

        // Move child cursor to row 1, col 16 (inside the top-right clock region 15..20)
        comp.step(Event::PtyOutput(b"\x1b[1;16H"), now);

        // Clock dodges to TopLeft (0..5)!
        assert_eq!(
            comp.rendered_text().lines().next().unwrap(),
            "14:30               "
        );
    }

    #[test]
    fn ipc_ttl_expires_slots_and_dynamic_overlays_automatically() {
        let t0 = Instant::now();
        let (mut comp, _) =
            Compositor::new(TermSize { cols: 20, rows: 4 }, sample_config(), t0).unwrap();

        comp.step(
            Event::Ipc(IpcMessage::SetSlot {
                key: "mode".to_string(),
                value: "BUILD".to_string(),
                ttl_ms: Some(1000),
            }),
            t0,
        );
        assert!(comp.rendered_text().contains("BUILD"));

        // Advance time by 1.5s -> slot expires
        let t1 = t0 + Duration::from_millis(1500);
        comp.step(
            Event::Tick {
                wall_clock: Local.with_ymd_and_hms(2026, 10, 22, 14, 30, 1).unwrap(),
                builtins: BuiltinValues::default(),
            },
            t1,
        );
        assert!(!comp.rendered_text().contains("BUILD"));
    }
}
