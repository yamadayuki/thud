use crate::compositor::vt::MouseEncoding;

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";
/// Lines scrolled per mouse wheel notch on the primary screen.
const WHEEL_LINES: usize = 3;

/// What the HUD should do in response to host input, in arrival order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputAction {
    ScrollUp(usize),
    ScrollDown(usize),
    /// Return the viewport to the live bottom of the scrollback.
    SnapBack,
    ToggleZen,
    /// Invert mouse capture, or return to the configured `mouse` mode.
    ToggleMouse,
    Reload,
    /// A line was submitted to the child (Enter outside a bracketed paste).
    Submit,
}

/// Host input split into bytes for the child and HUD actions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RoutedInput {
    pub to_child: Vec<u8>,
    pub actions: Vec<InputAction>,
}

/// The child and HUD state that decides what host input means.
#[derive(Debug, Clone, Copy)]
pub(crate) struct InputContext {
    pub zen_key: Option<u8>,
    pub reload_key: Option<u8>,
    pub mouse_key: Option<u8>,
    /// The child enabled mouse tracking, so mouse reports are forwarded to it.
    pub child_wants_mouse: bool,
    /// Encoding the child requested for forwarded mouse reports.
    pub child_mouse_encoding: MouseEncoding,
    pub alternate_screen: bool,
    /// Host row of the child viewport's top edge, subtracted from SGR mouse rows.
    pub child_top: u16,
}

/// Decides what each piece of host input means: bytes for the child, scrollback movement,
/// a HUD hotkey, or an opaque bracketed paste. Holds incomplete mouse reports until the
/// rest arrives in a later read.
#[derive(Debug, Default)]
pub(crate) struct InputRouter {
    pending: Vec<u8>,
    /// Inside a bracketed paste; counts how much of `PASTE_END` has been seen.
    paste_end_matched: Option<usize>,
}

impl InputRouter {
    pub fn route(&mut self, bytes: &[u8], ctx: &InputContext) -> RoutedInput {
        let mut input = std::mem::take(&mut self.pending);
        input.extend_from_slice(bytes);
        let mut out = RoutedInput::default();
        let mut forwarding = false;

        let mut i = 0;
        while i < input.len() {
            let rest = &input[i..];

            if let Some(matched) = self.paste_end_matched {
                let b = rest[0];
                out.to_child.push(b);
                let matched = if b == PASTE_END[matched] {
                    matched + 1
                } else if b == PASTE_END[0] {
                    1
                } else {
                    0
                };
                self.paste_end_matched = (matched < PASTE_END.len()).then_some(matched);
                i += 1;
                continue;
            }

            if rest.starts_with(PASTE_START) {
                out.actions.push(InputAction::SnapBack);
                out.to_child.extend_from_slice(PASTE_START);
                self.paste_end_matched = Some(0);
                forwarding = true;
                i += PASTE_START.len();
                continue;
            }

            if is_incomplete_mouse_report(rest) {
                self.pending = rest.to_vec();
                break;
            }

            if let Some(ev) = parse_mouse_event(rest) {
                route_mouse(&ev, ctx, &mut out);
                forwarding = false;
                i += ev.len;
                continue;
            }

            let b = rest[0];
            if Some(b) == ctx.zen_key {
                out.actions.push(InputAction::ToggleZen);
                forwarding = false;
            } else if Some(b) == ctx.reload_key {
                out.actions.push(InputAction::Reload);
                forwarding = false;
            } else if Some(b) == ctx.mouse_key {
                out.actions.push(InputAction::ToggleMouse);
                forwarding = false;
            } else {
                if !forwarding {
                    out.actions.push(InputAction::SnapBack);
                    forwarding = true;
                }
                out.to_child.push(b);
                if b == b'\r' || b == b'\n' {
                    out.actions.push(InputAction::Submit);
                }
            }
            i += 1;
        }
        out
    }
}

fn route_mouse(ev: &ParsedMouseEvent, ctx: &InputContext, out: &mut RoutedInput) {
    if ctx.child_wants_mouse {
        encode_for_child(ev, ctx, &mut out.to_child);
    } else if ctx.alternate_screen {
        match ev.kind {
            MouseEventKind::WheelUp => out.to_child.extend_from_slice(b"\x1b[A"),
            MouseEventKind::WheelDown => out.to_child.extend_from_slice(b"\x1b[B"),
            MouseEventKind::Other => {}
        }
    } else {
        match ev.kind {
            MouseEventKind::WheelUp => out.actions.push(InputAction::ScrollUp(WHEEL_LINES)),
            MouseEventKind::WheelDown => out.actions.push(InputAction::ScrollDown(WHEEL_LINES)),
            MouseEventKind::Other => {}
        }
    }
}

/// Re-encodes a host mouse report for the child: rows shift by the child viewport's top edge,
/// and the encoding follows what the child requested.
fn encode_for_child(ev: &ParsedMouseEvent, ctx: &InputContext, out: &mut Vec<u8>) {
    let row = ev.row.saturating_sub(ctx.child_top).max(1);
    match ctx.child_mouse_encoding {
        MouseEncoding::Sgr => {
            let suffix = if ev.release { 'm' } else { 'M' };
            let _ = std::io::Write::write_fmt(
                out,
                format_args!("\x1b[<{};{};{}{}", ev.btn, ev.col, row, suffix),
            );
        }
        encoding @ (MouseEncoding::Classic | MouseEncoding::Utf8) => {
            // Classic reports cannot say which button was released.
            let button = if ev.release {
                (ev.btn & !0b11) | 3
            } else {
                ev.btn
            };
            out.extend_from_slice(b"\x1b[M");
            for value in [button, ev.col, row] {
                let code = u32::from(value) + 32;
                if encoding == MouseEncoding::Classic {
                    out.push(code.min(255) as u8);
                } else {
                    let ch = char::from_u32(code.min(2047)).unwrap_or(' ');
                    let mut utf8 = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut utf8).as_bytes());
                }
            }
        }
    }
}

/// True when `bytes` is the start of an SGR (`ESC [ <`) or X10 (`ESC [ M`) mouse report that
/// has not fully arrived yet. A lone `ESC` or `ESC [` is not held, so the Esc key is never
/// delayed.
fn is_incomplete_mouse_report(bytes: &[u8]) -> bool {
    if let Some(body) = bytes.strip_prefix(b"\x1b[<") {
        body.iter().all(|b| b.is_ascii_digit() || *b == b';')
    } else {
        bytes.starts_with(b"\x1b[M") && bytes.len() < 6
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseEventKind {
    WheelUp,
    WheelDown,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedMouseEvent {
    kind: MouseEventKind,
    btn: u16,
    col: u16,
    row: u16,
    release: bool,
    is_sgr: bool,
    len: usize,
}

/// Parses an SGR (`\x1b[<btn;col;rowM`/`m`) or X10 (`\x1b[M<b1><b2><b3>`) mouse sequence
/// from the start of `bytes`.
fn parse_mouse_event(bytes: &[u8]) -> Option<ParsedMouseEvent> {
    if bytes.starts_with(b"\x1b[<") {
        let mut end = 3;
        while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b';') {
            end += 1;
        }
        if end < bytes.len() && (bytes[end] == b'M' || bytes[end] == b'm') {
            let release = bytes[end] == b'm';
            let body = std::str::from_utf8(&bytes[3..end]).ok()?;
            let mut parts = body.split(';');
            let btn: u16 = parts.next()?.parse().ok()?;
            let col: u16 = parts.next()?.parse().ok()?;
            let row: u16 = parts.next()?.parse().ok()?;
            if parts.next().is_some() {
                return None;
            }
            let kind = if !release && (btn & 64) != 0 {
                match btn & 3 {
                    0 => MouseEventKind::WheelUp,
                    1 => MouseEventKind::WheelDown,
                    _ => MouseEventKind::Other,
                }
            } else {
                MouseEventKind::Other
            };
            return Some(ParsedMouseEvent {
                kind,
                btn,
                col,
                row,
                release,
                is_sgr: true,
                len: end + 1,
            });
        }
    } else if bytes.starts_with(b"\x1b[M") && bytes.len() >= 6 {
        let btn = u16::from(bytes[3].saturating_sub(32));
        let col = u16::from(bytes[4].saturating_sub(32));
        let row = u16::from(bytes[5].saturating_sub(32));
        let kind = if (btn & 64) != 0 {
            match btn & 3 {
                0 => MouseEventKind::WheelUp,
                1 => MouseEventKind::WheelDown,
                _ => MouseEventKind::Other,
            }
        } else {
            MouseEventKind::Other
        };
        return Some(ParsedMouseEvent {
            kind,
            btn,
            col,
            row,
            release: false,
            is_sgr: false,
            len: 6,
        });
    }
    None
}
