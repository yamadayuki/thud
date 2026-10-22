use crate::config::{CollisionPolicy, Config, StatusBarPosition};
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
                        && (cell.width == 0 || !cell.ch.trim().is_empty())
                    {
                        return true;
                    }
                }
            }
        }
        false
    }
}
