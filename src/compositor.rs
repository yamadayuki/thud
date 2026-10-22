use crate::config::{
    Anchor, BorderStyle, CellStyle, CollisionPolicy, Color, Config, ConfigError, IpcMessage,
    OverlayConfig, ScreenCondition, TermSize, ToastLevel, WidgetKind, WidgetSpec,
};
use chrono::{DateTime, Local};
use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthChar;

const SPARK_BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
const MAX_METRIC_HISTORY: usize = 64;
const MAX_TOAST_STACK: usize = 5;

/// Built-in environment values passed into `Event::Tick`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuiltinValues {
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub battery: Option<String>,
    /// Active foreground process name inside the child PTY (None if idle at the root shell).
    pub fg_process: Option<String>,
}

/// All external occurrences that drive the Sans-I/O `Compositor` state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event<'a> {
    /// Raw input bytes read from the host terminal's stdin (intercepts HUD hotkeys).
    HostInput(&'a [u8]),
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
    /// Bytes to write to the child PTY master fd (forwarded user input + terminal query replies).
    pub child_pty_input: Vec<u8>,
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
            self.paint_char(row, col, ' ', 1, style);
            return;
        }

        // 1. Heal left neighbor if overwriting the continuation cell (2nd half) of a wide char.
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

        // 2. Heal right neighbor if overwriting the head cell (1st half) of a wide char with a 1-col char.
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

        if let Some(idx) = self.idx(row, col) {
            self.cells[idx] = RenderCell {
                ch: c.to_string(),
                width: char_width,
                style,
            };
        }

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

    /// Paints a string starting at `(row, start_col)`, clipped to `self.cols`.
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
struct MetricSeries {
    history: VecDeque<u8>,
    latest: u8,
    label: Option<String>,
    expires_at: Option<Instant>,
}

#[derive(Debug, Clone)]
struct ToastEntry {
    message: String,
    level: ToastLevel,
    expires_at: Instant,
}

#[derive(Debug, Clone)]
struct DynamicOverlayEntry {
    config: OverlayConfig,
    expires_at: Option<Instant>,
}

#[derive(Debug, Clone)]
struct StyledSegment {
    text: String,
    style: CellStyle,
}

/// Sans-I/O Deep Module that owns virtual terminal emulation, docked status-line geometry,
/// floating overlay layout, cursor/text collision dodging, CJK wide-character healing,
/// foreground process telemetry, toast stacking, sparkline/gauge metrics, hotkey toggles,
/// and minimal ANSI diff generation.
pub struct Compositor {
    host_size: TermSize,
    config: Config,
    vt: vt100::Parser,
    wall_clock: DateTime<Local>,
    builtins: BuiltinValues,
    active_fg_cmd: Option<(String, Instant)>,
    zen_mode: bool,
    dashboard_mode: bool,
    ipc_slots: BTreeMap<String, SlotEntry>,
    metrics: BTreeMap<String, MetricSeries>,
    toasts: VecDeque<ToastEntry>,
    command_outputs: BTreeMap<String, String>,
    command_last_run: BTreeMap<String, Instant>,
    dynamic_overlays: BTreeMap<String, DynamicOverlayEntry>,
    prev_frame: Option<FrameGrid>,
}

impl Compositor {
    /// Entry point 1: Initializes the `Compositor` and returns the initial `Transition`.
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

        let child_size = Self::compute_child_size(initial_host_size, &config, false);
        let vt = vt100::Parser::new(child_size.rows, child_size.cols, 0);

        let mut comp = Self {
            host_size: initial_host_size,
            config,
            vt,
            wall_clock: Local::now(),
            builtins: BuiltinValues::default(),
            active_fg_cmd: None,
            zen_mode: false,
            dashboard_mode: false,
            ipc_slots: BTreeMap::new(),
            metrics: BTreeMap::new(),
            toasts: VecDeque::new(),
            command_outputs: BTreeMap::new(),
            command_last_run: BTreeMap::new(),
            dynamic_overlays: BTreeMap::new(),
            prev_frame: None,
        };

        let due_commands = comp.collect_due_commands(now);
        let host_ansi_diff = comp.compose_and_diff(now);

        Ok((
            comp,
            Transition {
                host_ansi_diff,
                child_pty_input: Vec::new(),
                child_pty_resize: Some(child_size),
                due_commands,
            },
        ))
    }

    /// Entry point 2: Processes a single `Event` deterministically and returns the resulting `Transition`.
    /// This method is infallible.
    pub fn step(&mut self, event: Event<'_>, now: Instant) -> Transition {
        let mut child_pty_input = Vec::new();
        let mut child_pty_resize = None;
        let mut due_commands = Vec::new();

        match event {
            Event::HostInput(bytes) => {
                let zen_byte = Config::parse_hotkey_byte(self.config.zen_toggle_key.as_deref());
                let dash_byte =
                    Config::parse_hotkey_byte(self.config.dashboard_toggle_key.as_deref());

                for &b in bytes {
                    if Some(b) == zen_byte {
                        self.zen_mode = !self.zen_mode;
                        let new_child_size = self.child_size();
                        self.vt.set_size(new_child_size.rows, new_child_size.cols);
                        child_pty_resize = Some(new_child_size);
                    } else if Some(b) == dash_byte {
                        self.dashboard_mode = !self.dashboard_mode;
                    } else {
                        child_pty_input.push(b);
                    }
                }
            }
            Event::PtyOutput(bytes) => {
                self.vt.process(bytes);
                child_pty_input = self.respond_to_terminal_queries(bytes);
            }
            Event::HostResize(new_size) => {
                let clamped = TermSize {
                    cols: new_size.cols.max(1),
                    rows: new_size.rows.max(1),
                };
                if clamped != self.host_size {
                    self.host_size = clamped;
                    let new_child_size = self.child_size();
                    self.vt.set_size(new_child_size.rows, new_child_size.cols);
                    self.prev_frame = None;
                    child_pty_resize = Some(new_child_size);
                }
            }
            Event::Tick {
                wall_clock,
                builtins,
            } => {
                self.wall_clock = wall_clock;
                self.update_fg_telemetry(&builtins.fg_process, now);
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
                        self.metrics.remove(&key);
                    }
                    IpcMessage::PushMetric {
                        key,
                        value,
                        label,
                        ttl_ms,
                    } => {
                        let clamped = value.min(100);
                        let expires_at = ttl_ms.map(|ms| now + Duration::from_millis(ms));
                        let entry = self.metrics.entry(key).or_insert_with(|| MetricSeries {
                            history: VecDeque::with_capacity(MAX_METRIC_HISTORY),
                            latest: clamped,
                            label: None,
                            expires_at: None,
                        });
                        if entry.history.len() >= MAX_METRIC_HISTORY {
                            entry.history.pop_front();
                        }
                        entry.history.push_back(clamped);
                        entry.latest = clamped;
                        if label.is_some() {
                            entry.label = label;
                        }
                        entry.expires_at = expires_at;
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
        }

        let host_ansi_diff = self.compose_and_diff(now);
        Transition {
            host_ansi_diff,
            child_pty_input,
            child_pty_resize,
            due_commands,
        }
    }

    /// Returns the current composited screen as plain text (one line per row, separated by `\n`).
    pub fn rendered_text(&self) -> String {
        match &self.prev_frame {
            Some(frame) => frame.to_plain_text(),
            None => self.build_frame_grid(Instant::now()).to_plain_text(),
        }
    }

    /// Returns the child PTY dimensions calculated for the current host terminal size and Zen Mode state.
    pub fn child_size(&self) -> TermSize {
        Self::compute_child_size(self.host_size, &self.config, self.zen_mode)
    }

    /// Returns whether Zen Mode (all HUD hidden) is currently active.
    pub fn is_zen_mode(&self) -> bool {
        self.zen_mode
    }

    /// Returns whether Dashboard Mode is currently active.
    pub fn is_dashboard_mode(&self) -> bool {
        self.dashboard_mode
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

    fn respond_to_terminal_queries(&self, bytes: &[u8]) -> Vec<u8> {
        let mut reply = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] != 0x1b || i + 1 >= bytes.len() {
                i += 1;
                continue;
            }
            match bytes[i + 1] {
                b'[' => {
                    let start = i + 2;
                    let mut end = start;
                    while end < bytes.len() && !(0x40..=0x7e).contains(&bytes[end]) {
                        end += 1;
                    }
                    if end < bytes.len() {
                        let params = &bytes[start..end];
                        let final_byte = bytes[end];
                        match (params, final_byte) {
                            (b"" | b"0", b'c') => {
                                reply.extend_from_slice(b"\x1b[?62;22c");
                            }
                            (b">" | b">0", b'c') => {
                                reply.extend_from_slice(b"\x1b[>0;10;1c");
                            }
                            (b"5", b'n') => {
                                reply.extend_from_slice(b"\x1b[0n");
                            }
                            (b"6", b'n') => {
                                let (r, c) = self.vt.screen().cursor_position();
                                let _ = std::io::Write::write_fmt(
                                    &mut reply,
                                    format_args!("\x1b[{};{}R", r + 1, c + 1),
                                );
                            }
                            (b"?6", b'n') => {
                                let (r, c) = self.vt.screen().cursor_position();
                                let _ = std::io::Write::write_fmt(
                                    &mut reply,
                                    format_args!("\x1b[?{};{}R", r + 1, c + 1),
                                );
                            }
                            (b"?", b'u') => {
                                reply.extend_from_slice(b"\x1b[?0u");
                            }
                            (b">" | b">0", b'q') => {
                                reply.extend_from_slice(b"\x1bP>|thud(0.1.0)\x1b\\");
                            }
                            _ => {}
                        }
                        i = end + 1;
                        continue;
                    }
                }
                b']' => {
                    let start = i + 2;
                    let mut end = start;
                    while end < bytes.len() {
                        if bytes[end] == 0x07 {
                            break;
                        }
                        if bytes[end] == 0x1b && end + 1 < bytes.len() && bytes[end + 1] == b'\\' {
                            break;
                        }
                        end += 1;
                    }
                    if end < bytes.len() {
                        let osc_body = &bytes[start..end];
                        if osc_body == b"10;?" {
                            reply.extend_from_slice(b"\x1b]10;rgb:e5e5/e5e5/e5e5\x1b\\");
                        } else if osc_body == b"11;?" {
                            reply.extend_from_slice(b"\x1b]11;rgb:1c1c/1c1c/1c1c\x1b\\");
                        }
                        i = end + 1;
                        continue;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        reply
    }

    fn compute_child_size(host_size: TermSize, config: &Config, zen_mode: bool) -> TermSize {
        if zen_mode {
            return TermSize {
                cols: host_size.cols.max(1),
                rows: host_size.rows.max(1),
            };
        }
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
        self.metrics
            .retain(|_, entry| entry.expires_at.is_none_or(|exp| exp > now));
        self.toasts.retain(|t| t.expires_at > now);
        self.dynamic_overlays
            .retain(|_, entry| entry.expires_at.is_none_or(|exp| exp > now));
    }

    fn collect_due_commands(&mut self, now: Instant) -> Vec<(String, String)> {
        let mut specs = Vec::new();
        for w in &self.config.status_bar.left {
            specs.push(&w.kind);
        }
        for w in &self.config.status_bar.center {
            specs.push(&w.kind);
        }
        for w in &self.config.status_bar.right {
            specs.push(&w.kind);
        }
        for ov in &self.config.overlays {
            specs.push(&ov.widget.kind);
        }
        for ov in self.dynamic_overlays.values() {
            specs.push(&ov.config.widget.kind);
        }

        let mut due = Vec::new();
        for kind in specs {
            if let WidgetKind::Command {
                id,
                cmd,
                interval_ms,
            } = kind
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

    fn evaluate_widget(&self, spec: &WidgetSpec, now: Instant) -> String {
        let raw = match &spec.kind {
            WidgetKind::Clock { format } => self.wall_clock.format(format).to_string(),
            WidgetKind::Cwd => self.builtins.cwd.clone().unwrap_or_default(),
            WidgetKind::GitBranch => self.builtins.git_branch.clone().unwrap_or_default(),
            WidgetKind::Battery => self.builtins.battery.clone().unwrap_or_default(),
            WidgetKind::ActiveCommand { min_secs } => {
                if let Some((name, started)) = &self.active_fg_cmd {
                    let secs = now.saturating_duration_since(*started).as_secs();
                    if secs >= *min_secs {
                        format!("⚙ {name} ({secs}s)")
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                }
            }
            WidgetKind::Text { text } => text.clone(),
            WidgetKind::Command { id, .. } => {
                self.command_outputs.get(id).cloned().unwrap_or_default()
            }
            WidgetKind::IpcSlot { key } => self
                .ipc_slots
                .get(key)
                .map(|s| s.value.clone())
                .unwrap_or_default(),
            WidgetKind::Sparkline { key, width } => {
                if let Some(series) = self.metrics.get(key) {
                    Self::format_sparkline(&series.history, *width)
                } else {
                    String::new()
                }
            }
            WidgetKind::Gauge { key, width, label } => {
                if let Some(series) = self.metrics.get(key) {
                    let effective_label = series.label.as_deref().or(label.as_deref());
                    Self::format_gauge(series.latest, *width, effective_label)
                } else {
                    String::new()
                }
            }
        };

        if raw.is_empty() {
            return String::new();
        }
        let prefix = spec.style.prefix.as_deref().unwrap_or("");
        let suffix = spec.style.suffix.as_deref().unwrap_or("");
        format!("{prefix}{raw}{suffix}")
    }

    fn format_sparkline(history: &VecDeque<u8>, width: usize) -> String {
        if history.is_empty() || width == 0 {
            return String::new();
        }
        let mut out = String::new();
        let len = history.len();
        let pad = width.saturating_sub(len);
        for _ in 0..pad {
            out.push(SPARK_BLOCKS[0]);
        }
        for &val in history.iter().skip(len.saturating_sub(width)) {
            let idx = (usize::from(val.min(100)) * 7) / 100;
            out.push(SPARK_BLOCKS[idx]);
        }
        out
    }

    fn format_gauge(percent: u8, width: usize, label: Option<&str>) -> String {
        let clamped = percent.min(100);
        let filled = (usize::from(clamped) * width) / 100;
        let empty = width.saturating_sub(filled);
        let mut bar = String::new();
        for _ in 0..filled {
            bar.push('━');
        }
        for _ in 0..empty {
            bar.push('─');
        }
        match label {
            Some(lbl) if !lbl.is_empty() => format!("{lbl} {bar} {clamped}%"),
            _ => format!("{bar} {clamped}%"),
        }
    }

    fn build_frame_grid(&self, now: Instant) -> FrameGrid {
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

        // If Zen Mode is active, skip all HUD chrome rendering
        if self.zen_mode {
            return grid;
        }

        // 2. Render docked bottom status bar if enabled and terminal is tall enough
        let reserved = self.config.reserved_bottom_rows();
        if self.config.status_bar.enabled && self.host_size.rows > reserved && reserved > 0 {
            let status_row = self.host_size.rows - reserved;
            self.render_status_bar(&mut grid, status_row, reserved, now);
        }

        // 3. Render floating overlays sorted by z_index
        let mut overlays: Vec<&OverlayConfig> = self.config.overlays.iter().collect();
        for dyn_ov in self.dynamic_overlays.values() {
            overlays.push(&dyn_ov.config);
        }
        overlays.sort_by_key(|ov| ov.z_index);

        let is_alt = screen.alternate_screen();
        for ov in overlays {
            self.render_overlay(&mut grid, ov, child_size, is_alt, now);
        }

        // 4. Render stacked toast notifications from bottom-right upward
        self.render_toast_stack(&mut grid, child_size);

        grid
    }

    fn render_status_bar(&self, grid: &mut FrameGrid, start_row: u16, height: u16, now: Instant) {
        let default_style = CellStyle {
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
                grid.paint_char(r, c, ' ', 1, default_style);
            }
        }

        let sep = &self.config.status_bar.separator;
        let left_segs = self.build_segments(&self.config.status_bar.left, sep, default_style, now);
        let center_segs =
            self.build_segments(&self.config.status_bar.center, sep, default_style, now);
        let right_segs =
            self.build_segments(&self.config.status_bar.right, sep, default_style, now);

        // Paint left segments
        let mut col = 0;
        for seg in &left_segs {
            col += grid.paint_str(start_row, col, &seg.text, seg.style);
        }

        // Paint center segments
        let center_width: u16 = center_segs
            .iter()
            .map(|s| Self::display_width(&s.text))
            .sum();
        if center_width > 0 {
            let mut c_col = grid.cols.saturating_sub(center_width) / 2;
            for seg in &center_segs {
                c_col += grid.paint_str(start_row, c_col, &seg.text, seg.style);
            }
        }

        // Paint right segments
        let right_width: u16 = right_segs
            .iter()
            .map(|s| Self::display_width(&s.text))
            .sum();
        if right_width > 0 {
            let mut r_col = grid.cols.saturating_sub(right_width);
            for seg in &right_segs {
                r_col += grid.paint_str(start_row, r_col, &seg.text, seg.style);
            }
        }
    }

    fn build_segments(
        &self,
        specs: &[WidgetSpec],
        sep: &str,
        default_style: CellStyle,
        now: Instant,
    ) -> Vec<StyledSegment> {
        let mut out = Vec::new();
        for spec in specs {
            let text = self.evaluate_widget(spec, now);
            if text.is_empty() {
                continue;
            }
            if !out.is_empty() && !sep.is_empty() {
                out.push(StyledSegment {
                    text: sep.to_string(),
                    style: default_style,
                });
            }
            let seg_style = CellStyle {
                fg: Color::Ansi(spec.style.fg_ansi.unwrap_or(self.config.status_bar.fg_ansi)),
                bg: Color::Ansi(spec.style.bg_ansi.unwrap_or(self.config.status_bar.bg_ansi)),
                bold: spec.style.bold.unwrap_or(false),
                italic: false,
                underline: false,
                inverse: false,
            };
            out.push(StyledSegment {
                text,
                style: seg_style,
            });
        }
        out
    }

    fn render_overlay(
        &self,
        grid: &mut FrameGrid,
        ov: &OverlayConfig,
        child_size: TermSize,
        is_alt: bool,
        now: Instant,
    ) {
        match ov.screen_mode {
            ScreenCondition::Always => {}
            ScreenCondition::PrimaryOnly if is_alt => return,
            ScreenCondition::AlternateOnly if !is_alt => return,
            ScreenCondition::DashboardOnly if !self.dashboard_mode => return,
            _ => {}
        }

        let raw_text = self.evaluate_widget(&ov.widget, now);
        if raw_text.is_empty() {
            return;
        }

        let lines = Self::format_bordered_lines(&raw_text, ov.border, ov.title.as_deref());
        let box_height = lines.len() as u16;
        let box_width = lines
            .iter()
            .map(|l| Self::display_width(l))
            .max()
            .unwrap_or(0);

        if box_width == 0
            || box_height == 0
            || box_width > child_size.cols
            || box_height > child_size.rows
        {
            return;
        }

        let (mut row, mut col) = Self::resolve_box_anchor(
            ov.anchor,
            box_width,
            box_height,
            child_size.cols,
            child_size.rows,
            ov.offset_x,
            ov.offset_y,
        );

        match ov.collision {
            CollisionPolicy::Ignore => {}
            CollisionPolicy::AutoHide { margin, avoid_text } => {
                if Self::box_has_collision(
                    grid, row, col, box_width, box_height, margin, avoid_text,
                ) {
                    return;
                }
            }
            CollisionPolicy::DodgeToOppositeCorner { margin, avoid_text } => {
                if Self::box_has_collision(
                    grid, row, col, box_width, box_height, margin, avoid_text,
                ) {
                    let mut candidates = Vec::new();
                    let opposite = ov.anchor.opposite_horizontal();
                    candidates.push(Self::resolve_box_anchor(
                        opposite,
                        box_width,
                        box_height,
                        child_size.cols,
                        child_size.rows,
                        -ov.offset_x,
                        ov.offset_y,
                    ));
                    for r in (row + 1)..=child_size.rows.saturating_sub(box_height) {
                        candidates.push((r, col));
                    }
                    if row > 0 {
                        for r in (0..row).rev() {
                            candidates.push((r, col));
                        }
                    }

                    if let Some((free_r, free_c)) =
                        candidates.into_iter().find(|&(cand_r, cand_c)| {
                            !Self::box_has_collision(
                                grid, cand_r, cand_c, box_width, box_height, margin, avoid_text,
                            )
                        })
                    {
                        row = free_r;
                        col = free_c;
                    } else if avoid_text {
                        return;
                    } else {
                        let (dodged_row, dodged_col) = Self::resolve_box_anchor(
                            opposite,
                            box_width,
                            box_height,
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
        }

        let style = CellStyle {
            fg: Color::Ansi(ov.fg_ansi),
            bg: Color::Ansi(ov.bg_ansi),
            bold: ov.bold,
            italic: false,
            underline: false,
            inverse: false,
        };

        for (idx, line) in lines.iter().enumerate() {
            grid.paint_str(row + idx as u16, col, line, style);
        }
    }

    fn render_toast_stack(&self, grid: &mut FrameGrid, child_size: TermSize) {
        let mut current_row = i32::from(child_size.rows) - 1;

        // 1. Render any active unreferenced progress metrics (`thud progress <key> <pct>`)
        for (key, series) in &self.metrics {
            if current_row < 0 {
                break;
            }
            if self.is_metric_referenced_in_config(key) {
                continue;
            }
            let label = series.label.as_deref().unwrap_or(key.as_str());
            let gauge_str = Self::format_gauge(series.latest, 10, Some(label));
            let text = format!(" {gauge_str} ");
            let width = Self::display_width(&text);
            if width == 0 || width > child_size.cols {
                continue;
            }
            let col = child_size.cols - width;
            let style = CellStyle {
                fg: Color::Ansi(231),
                bg: Color::Ansi(25),
                bold: true,
                italic: false,
                underline: false,
                inverse: false,
            };
            grid.paint_str(current_row as u16, col, &text, style);
            current_row -= 1;
        }

        // 2. Render stacked toast notifications
        for toast in self.toasts.iter().rev() {
            if current_row < 0 {
                break;
            }
            let (fg_ansi, bg_ansi, icon) = toast.level.palette();
            let text = format!(" {icon}{} ", toast.message);
            let width = Self::display_width(&text);
            if width == 0 || width > child_size.cols {
                continue;
            }
            let col = child_size.cols - width;
            let style = CellStyle {
                fg: Color::Ansi(fg_ansi),
                bg: Color::Ansi(bg_ansi),
                bold: true,
                italic: false,
                underline: false,
                inverse: false,
            };
            grid.paint_str(current_row as u16, col, &text, style);
            current_row -= 1;
        }
    }

    fn is_metric_referenced_in_config(&self, target_key: &str) -> bool {
        let all_widgets = self
            .config
            .status_bar
            .left
            .iter()
            .chain(self.config.status_bar.center.iter())
            .chain(self.config.status_bar.right.iter())
            .chain(self.config.overlays.iter().map(|ov| &ov.widget))
            .chain(self.dynamic_overlays.values().map(|ov| &ov.config.widget));

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

    fn format_bordered_lines(
        raw_text: &str,
        border: BorderStyle,
        title: Option<&str>,
    ) -> Vec<String> {
        let content_lines: Vec<&str> = raw_text.lines().collect();
        let Some((tl, tr, bl, br, hz, vt)) = border.chars() else {
            return content_lines.into_iter().map(String::from).collect();
        };

        let max_content_width = content_lines
            .iter()
            .map(|l| usize::from(Self::display_width(l)))
            .max()
            .unwrap_or(0);
        let title_str = title.unwrap_or("");
        let title_width = usize::from(Self::display_width(title_str));
        let inner_width = max_content_width.max(title_width);

        let mut out = Vec::with_capacity(content_lines.len() + 2);

        // Top border with optional embedded title
        let mut top = String::new();
        top.push(tl);
        if title_width > 0 && title_width <= inner_width {
            top.push_str(title_str);
            for _ in 0..(inner_width - title_width) {
                top.push(hz);
            }
        } else {
            for _ in 0..inner_width {
                top.push(hz);
            }
        }
        top.push(tr);
        out.push(top);

        // Content rows
        for line in content_lines {
            let w = usize::from(Self::display_width(line));
            let mut row_str = String::new();
            row_str.push(vt);
            row_str.push_str(line);
            for _ in 0..inner_width.saturating_sub(w) {
                row_str.push(' ');
            }
            row_str.push(vt);
            out.push(row_str);
        }

        // Bottom border
        let mut bot = String::new();
        bot.push(bl);
        for _ in 0..inner_width {
            bot.push(hz);
        }
        bot.push(br);
        out.push(bot);

        out
    }

    fn resolve_box_anchor(
        anchor: Anchor,
        width: u16,
        height: u16,
        cols: u16,
        rows: u16,
        offset_x: i16,
        offset_y: i16,
    ) -> (u16, u16) {
        let base_row: i32 = match anchor {
            Anchor::TopLeft | Anchor::TopCenter | Anchor::TopRight => 0,
            Anchor::BottomLeft | Anchor::BottomCenter | Anchor::BottomRight => {
                i32::from(rows.saturating_sub(height))
            }
        };
        let base_col: i32 = match anchor {
            Anchor::TopLeft | Anchor::BottomLeft => 0,
            Anchor::TopCenter | Anchor::BottomCenter => i32::from(cols.saturating_sub(width) / 2),
            Anchor::TopRight | Anchor::BottomRight => i32::from(cols.saturating_sub(width)),
        };

        let r = (base_row + i32::from(offset_y)).clamp(0, i32::from(rows.saturating_sub(height)))
            as u16;
        let c =
            (base_col + i32::from(offset_x)).clamp(0, i32::from(cols.saturating_sub(width))) as u16;
        (r, c)
    }

    fn box_has_collision(
        grid: &FrameGrid,
        ov_row: u16,
        ov_col: u16,
        ov_width: u16,
        ov_height: u16,
        margin: u16,
        avoid_text: bool,
    ) -> bool {
        let min_r = ov_row.saturating_sub(margin);
        let max_r = ov_row
            .saturating_add(ov_height.saturating_sub(1))
            .saturating_add(margin);
        let min_c = ov_col.saturating_sub(margin);
        let max_c = ov_col.saturating_add(ov_width).saturating_add(margin);

        if grid.cursor_row >= min_r
            && grid.cursor_row <= max_r
            && grid.cursor_col >= min_c
            && grid.cursor_col < max_c
        {
            return true;
        }

        if avoid_text {
            for r in ov_row..(ov_row + ov_height).min(grid.rows) {
                for c in ov_col..(ov_col + ov_width).min(grid.cols) {
                    if let Some(cell) = grid.get(r, c)
                        && (cell.width == 0 || !cell.ch.trim().is_empty())
                    {
                        return true;
                    }
                }
            }
        }
        false
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

    fn compose_and_diff(&mut self, now: Instant) -> Vec<u8> {
        let next = self.build_frame_grid(now);
        let mut ansi = String::new();

        let mut any_cell_changed = false;
        let mut current_style: Option<CellStyle> = None;

        for r in 0..next.rows {
            let mut c = 0;
            while c < next.cols {
                let next_cell =
                    &next.cells[usize::from(r) * usize::from(next.cols) + usize::from(c)];
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

        assert_eq!(
            init_tx.child_pty_resize,
            Some(TermSize { cols: 20, rows: 3 })
        );
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
                    fg_process: None,
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
        let mut cfg = sample_config();
        cfg.overlays[0].collision = CollisionPolicy::Ignore;
        let (mut comp, _) = Compositor::new(TermSize { cols: 20, rows: 4 }, cfg, now).unwrap();

        let fixed_time = Local.with_ymd_and_hms(2026, 10, 22, 9, 5, 0).unwrap();
        comp.step(
            Event::Tick {
                wall_clock: fixed_time,
                builtins: BuiltinValues::default(),
            },
            now,
        );

        comp.step(
            Event::PtyOutput(b"\x1b[?1049h\x1b[1;11H\xe3\x81\x82\xe3\x81\x84\xe3\x81\x86\xe3\x81\x88\xe3\x81\x8a\x1b[1;1H"),
            now,
        );

        assert_eq!(
            comp.rendered_text().lines().next().unwrap(),
            "          あい 09:05"
        );

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

        comp.step(Event::PtyOutput(b"\x1b[1;16H"), now);

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

    #[test]
    fn dodges_underlying_text_when_first_line_reaches_top_right() {
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

        comp.step(Event::PtyOutput(b"0123456789abcdefghij\r\nOK"), now);

        let rendered = comp.rendered_text();
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines[0], "0123456789abcdefghij");
        assert_eq!(lines[1], "OK             14:30");
    }

    #[test]
    fn responds_to_fish_and_vt_terminal_queries() {
        let now = Instant::now();
        let (mut comp, _) =
            Compositor::new(TermSize { cols: 80, rows: 24 }, sample_config(), now).unwrap();

        let tx = comp.step(
            Event::PtyOutput(b"hello\x1b[?u\x1b[>0q\x1b]11;?\x1b\\\x1b[0c\x1b[6n"),
            now,
        );
        let reply = String::from_utf8_lossy(&tx.child_pty_input);
        assert!(reply.contains("\x1b[?0u"));
        assert!(reply.contains("\x1bP>|thud(0.1.0)\x1b\\"));
        assert!(reply.contains("\x1b]11;rgb:1c1c/1c1c/1c1c\x1b\\"));
        assert!(reply.contains("\x1b[?62;22c"));
        assert!(reply.contains("\x1b[1;6R"));
    }

    #[test]
    fn renders_bordered_overlay_sparkline_gauge_and_toast_stack() {
        let now = Instant::now();
        let cfg = Config::from_toml(
            r#"
            [status_bar]
            enabled = true
            height = 1
            separator = " "
            left = [
                { type = "sparkline", key = "cpu", width = 4 },
                { type = "gauge", key = "build", width = 4 }
            ]
            right = []

            [[overlays]]
            id = "sys"
            anchor = "top_right"
            border = "rounded"
            title = "SYS"
            collision = { mode = "ignore" }
            widget = { type = "text", text = "OK" }
        "#,
        )
        .unwrap();

        let (mut comp, _) = Compositor::new(TermSize { cols: 24, rows: 6 }, cfg, now).unwrap();

        comp.step(
            Event::Ipc(IpcMessage::PushMetric {
                key: "cpu".to_string(),
                value: 0,
                label: None,
                ttl_ms: None,
            }),
            now,
        );
        comp.step(
            Event::Ipc(IpcMessage::PushMetric {
                key: "cpu".to_string(),
                value: 100,
                label: None,
                ttl_ms: None,
            }),
            now,
        );
        comp.step(
            Event::Ipc(IpcMessage::PushMetric {
                key: "build".to_string(),
                value: 50,
                label: Some("B".to_string()),
                ttl_ms: None,
            }),
            now,
        );
        comp.step(
            Event::Ipc(IpcMessage::Toast {
                message: "T1".to_string(),
                level: ToastLevel::Info,
                ttl_ms: 4000,
            }),
            now,
        );
        comp.step(
            Event::Ipc(IpcMessage::Toast {
                message: "T2".to_string(),
                level: ToastLevel::Success,
                ttl_ms: 4000,
            }),
            now,
        );

        let rendered = comp.rendered_text();
        let lines: Vec<&str> = rendered.lines().collect();
        // Top-right 3-line rounded border box: ╭SYS╮ / │OK │ / ╰───╯
        assert_eq!(lines[0], "                   ╭SYS╮");
        assert_eq!(lines[1], "                   │OK │");
        assert_eq!(lines[2], "                   ╰───╯");
        // Stacked toasts at bottom-right of child area (rows 3 and 4)
        assert!(lines[3].ends_with(" ℹ T1 "));
        assert!(lines[4].ends_with(" ✓ T2 "));
        // Bottom status bar with sparkline & gauge
        assert!(lines[5].contains("▁▁▁█ B ━━── 50%"));
    }

    #[test]
    fn intercepts_zen_mode_and_dashboard_hotkeys_and_tracks_active_command() {
        let t0 = Instant::now();
        let cfg = Config::from_toml(
            r#"
            zen_toggle_key = "ctrl-\\"
            dashboard_toggle_key = "ctrl-]"

            [status_bar]
            height = 1
            left = [{ type = "active_command", min_secs = 2 }]
            right = []

            [[overlays]]
            id = "dash"
            anchor = "top_left"
            screen_mode = "dashboard_only"
            collision = { mode = "ignore" }
            widget = { type = "text", text = "DASHBOARD" }
        "#,
        )
        .unwrap();

        let (mut comp, _) = Compositor::new(TermSize { cols: 20, rows: 4 }, cfg, t0).unwrap();

        // 1. Foreground process "cargo" starts at t0 -> not shown yet (min_secs = 2)
        comp.step(
            Event::Tick {
                wall_clock: Local::now(),
                builtins: BuiltinValues {
                    fg_process: Some("cargo".to_string()),
                    ..Default::default()
                },
            },
            t0,
        );
        // At t0 + 3s, "cargo" has been running for 3s -> shown on status bar!
        let t3 = t0 + Duration::from_secs(3);
        comp.step(
            Event::Tick {
                wall_clock: Local::now(),
                builtins: BuiltinValues {
                    fg_process: Some("cargo".to_string()),
                    ..Default::default()
                },
            },
            t3,
        );
        assert!(comp.rendered_text().contains("⚙ cargo"));

        // 2. Toggle Dashboard Mode via Ctrl+] (0x1d) -> "DASHBOARD" overlay appears!
        let tx_dash = comp.step(Event::HostInput(b"a\x1db"), t3);
        assert_eq!(tx_dash.child_pty_input, b"ab");
        assert!(comp.is_dashboard_mode());
        assert!(comp.rendered_text().contains("DASHBOARD"));

        // 3. Toggle Zen Mode via Ctrl+\ (0x1c) -> HUD hidden and child PTY expands to 20x4!
        let tx_zen = comp.step(Event::HostInput(b"\x1c"), t3);
        assert!(comp.is_zen_mode());
        assert_eq!(
            tx_zen.child_pty_resize,
            Some(TermSize { cols: 20, rows: 4 })
        );
        assert!(!comp.rendered_text().contains("DASHBOARD"));
        assert!(!comp.rendered_text().contains("⚙ cargo"));
    }
}
