use crate::config::{CollisionPolicy, Config, StatusBarPosition, ToastConfig};
use crate::ui::{Anchor, Buffer, Rect, TermSize};

/// Pure geometric solver for child PTY viewport sizing and floating overlay collision avoidance.
pub(crate) struct PlacementSolver;

impl PlacementSolver {
    /// Calculates the child PTY viewport rectangle inside the host terminal `Buffer` after reserving
    /// top or bottom rows for the docked status bar (unless Zen Mode is active).
    pub fn compute_child_rect(host_size: TermSize, config: &Config, zen_mode: bool) -> Rect {
        let cols = host_size.cols.max(1);
        let rows = host_size.rows.max(1);
        if zen_mode {
            return Rect::new(0, 0, cols, rows);
        }
        let reserved = config.reserved_status_rows();
        if rows > reserved && reserved > 0 {
            let child_rows = rows - reserved;
            let top_y = match config.status_bar.position {
                StatusBarPosition::Bottom => 0,
                StatusBarPosition::Top => reserved,
            };
            Rect::new(0, top_y, cols, child_rows)
        } else {
            Rect::new(0, 0, cols, rows)
        }
    }

    /// Calculates the child PTY dimensions after reserving rows for the docked status bar
    /// (unless Zen Mode is active).
    pub fn compute_child_size(host_size: TermSize, config: &Config, zen_mode: bool) -> TermSize {
        let rect = Self::compute_child_rect(host_size, config, zen_mode);
        TermSize {
            cols: rect.width,
            rows: rect.height,
        }
    }

    /// Resolves the target `Rect` for a floating overlay of `(box_width, box_height)` within
    /// `child_rect`, applying `policy` (`Ignore`, `AutoHide`, `DodgeToOppositeCorner`) against
    /// the cursor and underlying cells in `buf`.
    #[allow(clippy::too_many_arguments)]
    pub fn resolve_overlay_rect(
        buf: &Buffer,
        child_rect: Rect,
        anchor: Anchor,
        offset_x: i16,
        offset_y: i16,
        box_width: u16,
        box_height: u16,
        policy: CollisionPolicy,
    ) -> Option<Rect> {
        if box_width == 0
            || box_height == 0
            || box_width > child_rect.width
            || box_height > child_rect.height
        {
            return None;
        }

        let (mut row, mut col) = Self::resolve_box_anchor(
            anchor, box_width, box_height, child_rect, offset_x, offset_y,
        );

        match policy {
            CollisionPolicy::Ignore => {}
            CollisionPolicy::AutoHide { margin, avoid_text } => {
                if Self::box_has_collision(buf, row, col, box_width, box_height, margin, avoid_text)
                {
                    return None;
                }
            }
            CollisionPolicy::DodgeToOppositeCorner { margin, avoid_text } => {
                if Self::box_has_collision(buf, row, col, box_width, box_height, margin, avoid_text)
                {
                    let mut candidates = Vec::new();
                    let opposite = anchor.opposite_horizontal();
                    candidates.push(Self::resolve_box_anchor(
                        opposite, box_width, box_height, child_rect, -offset_x, offset_y,
                    ));
                    for r in (row + 1)..=child_rect.bottom().saturating_sub(box_height) {
                        candidates.push((r, col));
                    }
                    if row > child_rect.y {
                        for r in (child_rect.y..row).rev() {
                            candidates.push((r, col));
                        }
                    }

                    if let Some((free_r, free_c)) =
                        candidates.into_iter().find(|&(cand_r, cand_c)| {
                            !Self::box_has_collision(
                                buf, cand_r, cand_c, box_width, box_height, margin, avoid_text,
                            )
                        })
                    {
                        row = free_r;
                        col = free_c;
                    } else if avoid_text {
                        return None;
                    } else {
                        let (dodged_row, dodged_col) = Self::resolve_box_anchor(
                            opposite, box_width, box_height, child_rect, -offset_x, offset_y,
                        );
                        row = dodged_row;
                        col = dodged_col;
                    }
                }
            }
        }

        Some(Rect::new(col, row, box_width, box_height))
    }

    /// Places a stack of toast boxes of `sizes` (newest first) inside `child_rect` as one unit:
    /// toasts wider than the child area are skipped, the stack is cut where it no longer fits
    /// vertically, and the whole stack avoids collisions according to `toast.collision`.
    /// Returns the index into `sizes` and target `Rect` of every placed toast.
    pub fn resolve_toast_stack(
        buf: &Buffer,
        child_rect: Rect,
        toast: &ToastConfig,
        sizes: &[(u16, u16)],
    ) -> Vec<(usize, Rect)> {
        let max_height = child_rect.height.saturating_sub(toast.margin_y);
        let mut fitting: Vec<(usize, u16, u16)> = Vec::new();
        let mut stack_height: u16 = 0;
        for (i, &(w, h)) in sizes.iter().enumerate() {
            if w == 0 || h == 0 || w > child_rect.width {
                continue;
            }
            let needed = if fitting.is_empty() {
                h
            } else {
                stack_height.saturating_add(toast.gap).saturating_add(h)
            };
            if needed > max_height {
                break;
            }
            stack_height = needed;
            fitting.push((i, w, h));
        }
        let Some(stack_width) = fitting.iter().map(|&(_, w, _)| w).max() else {
            return Vec::new();
        };

        let margin_x = i16::try_from(toast.margin_x).unwrap_or(i16::MAX);
        let margin_y = i16::try_from(toast.margin_y).unwrap_or(i16::MAX);
        let offset_x = match toast.anchor {
            Anchor::TopRight | Anchor::BottomRight => -margin_x,
            Anchor::TopLeft | Anchor::BottomLeft => margin_x,
            Anchor::TopCenter | Anchor::BottomCenter => 0,
        };
        let offset_y = if is_top(toast.anchor) {
            margin_y
        } else {
            -margin_y
        };
        let Some(stack) = Self::resolve_overlay_rect(
            buf,
            child_rect,
            toast.anchor,
            offset_x,
            offset_y,
            stack_width,
            stack_height,
            toast.collision,
        ) else {
            return Vec::new();
        };

        // Lay toasts out toward whichever corner the stack ended up at.
        let home = Self::resolve_box_anchor(
            toast.anchor,
            stack_width,
            stack_height,
            child_rect,
            offset_x,
            offset_y,
        );
        let flipped = toast.anchor.opposite_horizontal();
        let dodged = Self::resolve_box_anchor(
            flipped,
            stack_width,
            stack_height,
            child_rect,
            -offset_x,
            offset_y,
        );
        let anchor = if (stack.y, stack.x) != home && (stack.y, stack.x) == dodged {
            flipped
        } else {
            toast.anchor
        };

        let mut placed = Vec::with_capacity(fitting.len());
        let mut row = if is_top(anchor) {
            stack.y
        } else {
            stack.bottom()
        };
        for (i, w, h) in fitting {
            let col = match anchor {
                Anchor::TopRight | Anchor::BottomRight => stack.right() - w,
                Anchor::TopLeft | Anchor::BottomLeft => stack.x,
                Anchor::TopCenter | Anchor::BottomCenter => stack.x + (stack_width - w) / 2,
            };
            let top_row = if is_top(anchor) {
                let top = row;
                row = row.saturating_add(h).saturating_add(toast.gap);
                top
            } else {
                row = row.saturating_sub(h);
                let top = row;
                row = row.saturating_sub(toast.gap);
                top
            };
            placed.push((i, Rect::new(col, top_row, w, h)));
        }
        placed
    }

    fn resolve_box_anchor(
        anchor: Anchor,
        width: u16,
        height: u16,
        child_rect: Rect,
        offset_x: i16,
        offset_y: i16,
    ) -> (u16, u16) {
        let base_row: i32 = match anchor {
            Anchor::TopLeft | Anchor::TopCenter | Anchor::TopRight => i32::from(child_rect.y),
            Anchor::BottomLeft | Anchor::BottomCenter | Anchor::BottomRight => {
                i32::from(child_rect.bottom().saturating_sub(height))
            }
        };
        let base_col: i32 = match anchor {
            Anchor::TopLeft | Anchor::BottomLeft => i32::from(child_rect.x),
            Anchor::TopCenter | Anchor::BottomCenter => {
                i32::from(child_rect.x + child_rect.width.saturating_sub(width) / 2)
            }
            Anchor::TopRight | Anchor::BottomRight => {
                i32::from(child_rect.right().saturating_sub(width))
            }
        };

        let min_row = i32::from(child_rect.y);
        let max_row = i32::from(child_rect.bottom().saturating_sub(height)).max(min_row);
        let min_col = i32::from(child_rect.x);
        let max_col = i32::from(child_rect.right().saturating_sub(width)).max(min_col);

        let r = (base_row + i32::from(offset_y)).clamp(min_row, max_row) as u16;
        let c = (base_col + i32::from(offset_x)).clamp(min_col, max_col) as u16;
        (r, c)
    }

    fn box_has_collision(
        buf: &Buffer,
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

        if buf.cursor.row >= min_r
            && buf.cursor.row <= max_r
            && buf.cursor.col >= min_c
            && buf.cursor.col < max_c
        {
            return true;
        }

        if avoid_text {
            for r in ov_row..(ov_row + ov_height).min(buf.rows) {
                for c in ov_col..(ov_col + ov_width).min(buf.cols) {
                    if let Some(cell) = buf.get(r, c)
                        && (cell.width == 0 || !cell.ch.is_whitespace())
                    {
                        return true;
                    }
                }
            }
        }
        false
    }
}

fn is_top(anchor: Anchor) -> bool {
    matches!(
        anchor,
        Anchor::TopRight | Anchor::TopLeft | Anchor::TopCenter
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toast(anchor: Anchor, collision: CollisionPolicy) -> ToastConfig {
        ToastConfig {
            anchor,
            margin_x: 2,
            margin_y: 1,
            gap: 1,
            collision,
            ..ToastConfig::default()
        }
    }

    fn cursor_at(row: u16, col: u16) -> Buffer {
        let mut buf = Buffer::new(40, 12);
        buf.cursor.row = row;
        buf.cursor.col = col;
        buf
    }

    const CHILD: Rect = Rect::new(0, 0, 40, 11);
    const DODGE: CollisionPolicy = CollisionPolicy::DodgeToOppositeCorner {
        margin: 1,
        avoid_text: false,
    };

    #[test]
    fn stacks_toasts_upward_from_bottom_right_skipping_and_cutting() {
        let placed = PlacementSolver::resolve_toast_stack(
            &cursor_at(0, 0),
            CHILD,
            &toast(Anchor::BottomRight, CollisionPolicy::Ignore),
            &[(10, 1), (50, 1), (6, 3), (8, 4), (8, 4)],
        );
        assert_eq!(
            placed,
            vec![
                (0, Rect::new(28, 9, 10, 1)),
                (2, Rect::new(32, 5, 6, 3)),
                (3, Rect::new(30, 0, 8, 4)),
            ],
            "too-wide toast skipped, newest at the bottom, stack cut when it overflows"
        );
    }

    #[test]
    fn whole_stack_dodges_or_hides_when_the_cursor_approaches() {
        let sizes = [(10, 1), (6, 1)];
        let near_bottom_right = cursor_at(9, 35);

        let dodged = PlacementSolver::resolve_toast_stack(
            &near_bottom_right,
            CHILD,
            &toast(Anchor::BottomRight, DODGE),
            &sizes,
        );
        assert_eq!(
            dodged,
            vec![(0, Rect::new(2, 9, 10, 1)), (1, Rect::new(2, 7, 6, 1))],
            "stack moves to the bottom-left and aligns to its new corner"
        );

        let hidden = PlacementSolver::resolve_toast_stack(
            &near_bottom_right,
            CHILD,
            &toast(
                Anchor::BottomRight,
                CollisionPolicy::AutoHide {
                    margin: 1,
                    avoid_text: false,
                },
            ),
            &sizes,
        );
        assert!(hidden.is_empty());

        let ignored = PlacementSolver::resolve_toast_stack(
            &near_bottom_right,
            CHILD,
            &toast(Anchor::BottomRight, CollisionPolicy::Ignore),
            &sizes,
        );
        assert_eq!(ignored[0].1, Rect::new(28, 9, 10, 1));
    }

    #[test]
    fn stacks_downward_from_top_anchors() {
        let placed = PlacementSolver::resolve_toast_stack(
            &cursor_at(11, 0),
            CHILD,
            &toast(Anchor::TopLeft, DODGE),
            &[(10, 1), (6, 2)],
        );
        assert_eq!(
            placed,
            vec![(0, Rect::new(2, 1, 10, 1)), (1, Rect::new(2, 3, 6, 2))]
        );
    }
}
