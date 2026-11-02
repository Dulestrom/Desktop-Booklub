//! How tiles share a row (REQ-46). Each tile has a minimum width. The row holds as many as fit,
//! and they grow to take up what is left, so the last one always ends at the right edge.

/// The columns that `available` pixels hold, for tiles at least `min_width` wide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fit {
    pub columns: i32,
    available: i32,
    gap: i32,
}

impl Fit {
    pub fn new(available: i32, min_width: i32, gap: i32) -> Self {
        let columns = ((available + gap) / (min_width + gap).max(1)).max(1);
        Self { columns, available: available.max(1), gap }
    }

    /// Whole pixels can't divide evenly, so each edge is rounded from the total. That way the
    /// rounding never adds up to a gap at the end of the row.
    pub fn x(&self, column: i32) -> i32 {
        column * (self.available + self.gap) / self.columns
    }

    pub fn width(&self, column: i32) -> i32 {
        (self.x(column + 1) - self.gap - self.x(column)).max(1)
    }

    /// One height for the whole row, so tiles that differ by a pixel in width still line up.
    pub fn height(&self, ratio: f64) -> i32 {
        (f64::from(self.width(0)) * ratio).round() as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_tile_always_ends_at_the_right_edge() {
        for available in 200..4000 {
            let fit = Fit::new(available, 148, 16);
            assert_eq!(fit.x(fit.columns - 1) + fit.width(fit.columns - 1), available, "at {available}");
        }
    }

    #[test]
    fn gaps_stay_exact_and_tiles_differ_by_at_most_a_pixel() {
        for available in 200..4000 {
            let fit = Fit::new(available, 148, 16);
            for column in 1..fit.columns {
                assert_eq!(fit.x(column), fit.x(column - 1) + fit.width(column - 1) + 16, "at {available}");
                assert!((fit.width(column) - fit.width(0)).abs() <= 1, "at {available}");
            }
        }
    }

    #[test]
    fn tiles_grow_until_a_new_one_fits() {
        // 5 x 148 + 4 x 16 = 804
        assert_eq!(Fit::new(803, 148, 16).columns, 4);
        assert!(Fit::new(803, 148, 16).width(0) > 148);
        assert_eq!(Fit::new(804, 148, 16).columns, 5);
        assert_eq!(Fit::new(804, 148, 16).width(0), 148);
    }

    #[test]
    fn tiles_never_shrink_below_the_minimum_while_a_row_can_hold_one() {
        for available in 148..4000 {
            assert!(Fit::new(available, 148, 16).width(0) >= 148, "at {available}");
        }
    }

    #[test]
    fn a_narrow_space_still_has_one_column() {
        assert_eq!(Fit::new(40, 148, 16).columns, 1);
        assert_eq!(Fit::new(0, 148, 16).width(0), 1);
    }

    #[test]
    fn the_height_follows_the_width() {
        assert_eq!(Fit::new(804, 148, 16).height(1.5), 222);
    }
}
