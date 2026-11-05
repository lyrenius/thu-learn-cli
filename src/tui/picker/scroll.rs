//! Cursor steps and scrolling with a scroll-off margin, ported from yazi.
//!
//! Ported from yazi (https://github.com/sxyazi/yazi) at commit
//! b4f0d123952e52c4dafe874178de2b34c82f1ab2: `Step::add` from
//! `yazi-widgets/src/step.rs`, the `Scrollable` trait from
//! `yazi-widgets/src/scrollable.rs`, and `squeeze_offset` from
//! `yazi-core/src/tab/folder.rs`. Changes: the trait became a plain struct and
//! the unused `Vp` step was dropped.
//!
//! Copyright (c) 2023 - sxyazi. Licensed under the MIT License; see the full
//! notice in `natsort.rs` in this directory.

/// A cursor movement. `Prev`/`Next` wrap around; every other step clamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Top,
    Bot,
    Prev,
    Next,
    Offset(isize),
    /// Percent of the visible height, e.g. 50 for half a page.
    Percent(i8),
}

impl Step {
    pub fn add(self, pos: usize, len: usize, limit: usize) -> usize {
        if len == 0 {
            return 0;
        }
        let off = match self {
            Self::Top => return 0,
            Self::Bot => return len - 1,
            Self::Prev => -1,
            Self::Next => 1,
            Self::Offset(n) => n,
            Self::Percent(0) => 0,
            Self::Percent(n) => n as isize * limit as isize / 100,
        };
        if matches!(self, Self::Prev | Self::Next) {
            off.saturating_add_unsigned(pos).rem_euclid(len as isize) as usize
        } else {
            pos.saturating_add_signed(off)
        }
        .min(len - 1)
    }
}

/// Cursor and first visible row of a list `limit` rows tall.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Scroll {
    pub cursor: usize,
    pub offset: usize,
}

impl Scroll {
    /// Moves by `step` over `total` items and keeps a `scrolloff` margin.
    /// Returns whether the cursor or offset changed.
    pub fn arrow(&mut self, step: Step, total: usize, limit: usize, scrolloff: usize) -> bool {
        let old = *self;
        if total == 0 {
            *self = Self::default();
            return old != *self;
        }
        let new = step.add(self.cursor, total, limit);
        if new > self.cursor {
            self.next(new, total, limit, scrolloff);
        } else {
            self.prev(new, total, scrolloff);
        }
        self.squeeze(total, limit, scrolloff);
        old != *self
    }

    fn next(&mut self, n_cur: usize, total: usize, limit: usize, scrolloff: usize) {
        let visible_end = total.min(self.offset + limit);
        self.offset = if n_cur >= visible_end.saturating_sub(scrolloff) {
            total
                .saturating_sub(limit)
                .min(self.offset + n_cur - self.cursor)
        } else {
            self.offset.min(total.saturating_sub(1))
        };
        self.cursor = n_cur;
    }

    fn prev(&mut self, n_cur: usize, total: usize, scrolloff: usize) {
        self.offset = if n_cur < self.offset + scrolloff {
            self.offset.saturating_sub(self.cursor - n_cur)
        } else {
            total.saturating_sub(1).min(self.offset)
        };
        self.cursor = n_cur;
    }

    fn squeeze(&mut self, total: usize, limit: usize, scrolloff: usize) {
        self.offset = if self.cursor < (self.offset + limit).min(total).saturating_sub(scrolloff) {
            total.saturating_sub(limit).min(self.offset)
        } else {
            total
                .saturating_sub(limit)
                .min(self.cursor.saturating_sub(limit) + 1 + scrolloff)
        }
        .min(self.cursor);
    }
}

/// yazi's file lists keep `min(limit / 2, 5)` rows above and below the cursor.
pub fn folder_scrolloff(limit: usize) -> usize {
    (limit / 2).min(5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prev_next_wrap_and_offsets_clamp() {
        assert_eq!(Step::Next.add(4, 5, 10), 0);
        assert_eq!(Step::Prev.add(0, 5, 10), 4);
        assert_eq!(Step::Offset(1).add(4, 5, 10), 4);
        assert_eq!(Step::Percent(50).add(0, 100, 21), 10);
        assert_eq!(Step::Percent(-100).add(5, 100, 20), 0);
        assert_eq!(Step::Bot.add(0, 0, 10), 0);
    }

    #[test]
    fn scrolling_keeps_a_five_row_margin() {
        let (total, limit) = (100, 20);
        let so = folder_scrolloff(limit);
        let mut s = Scroll::default();
        for _ in 0..14 {
            s.arrow(Step::Next, total, limit, so);
        }
        assert_eq!(
            s,
            Scroll {
                cursor: 14,
                offset: 0
            }
        );
        s.arrow(Step::Next, total, limit, so);
        assert_eq!(
            s,
            Scroll {
                cursor: 15,
                offset: 1
            }
        );
        // Matches the worked examples in yazi's behaviour: C-d twice, then G and gg.
        let mut s = Scroll::default();
        s.arrow(Step::Percent(50), total, limit, so);
        assert_eq!(
            s,
            Scroll {
                cursor: 10,
                offset: 0
            }
        );
        s.arrow(Step::Percent(50), total, limit, so);
        assert_eq!(
            s,
            Scroll {
                cursor: 20,
                offset: 10
            }
        );
        s.arrow(Step::Bot, total, limit, so);
        assert_eq!(
            s,
            Scroll {
                cursor: 99,
                offset: 80
            }
        );
        s.arrow(Step::Top, total, limit, so);
        assert_eq!(
            s,
            Scroll {
                cursor: 0,
                offset: 0
            }
        );
        // Wrapping up from the top shows the last page.
        s.arrow(Step::Prev, total, limit, so);
        assert_eq!(
            s,
            Scroll {
                cursor: 99,
                offset: 80
            }
        );
    }
}
