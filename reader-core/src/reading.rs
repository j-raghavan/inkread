//! Reading-time estimate (#98): a rough "time left" from the pages still ahead.
//!
//! Deliberately crude: a fixed per-page cost, not a measurement. A device page holds a different
//! amount of text in a reflowed EPUB than in a fixed-layout PDF, so this is an orientation aid, not
//! a clock. Deriving the per-page cost from the reader's observed turn cadence is the obvious later
//! refinement (#98); the policy lives here, host-tested, so that change is a one-place edit.

/// Seconds to read one device page at the default font. A single rough constant to start from.
pub const DEFAULT_SECS_PER_PAGE: u32 = 50;

/// Estimated whole minutes to finish `pages_left` pages at `secs_per_page` each, rounded to the
/// nearest minute but never rounding a non-zero estimate down to zero. Returns `0` when nothing is
/// left to read.
///
/// Signed `i32` across the JNI boundary; a non-positive `pages_left` (already at or past the end) or
/// a non-positive `secs_per_page` yields `0` rather than panicking (RR21-FR3), and the arithmetic is
/// done in `i64` so a pathological page count saturates instead of overflowing.
#[must_use]
pub fn minutes_left(pages_left: i32, secs_per_page: i32) -> i32 {
    if pages_left <= 0 || secs_per_page <= 0 {
        return 0;
    }
    let total_secs = i64::from(pages_left) * i64::from(secs_per_page);
    let minutes = (total_secs + 30) / 60;
    minutes.clamp(1, i64::from(i32::MAX)) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_left_is_zero() {
        assert_eq!(minutes_left(0, 50), 0);
        assert_eq!(minutes_left(-3, 50), 0, "past the end is not negative time");
    }

    #[test]
    fn a_non_zero_estimate_is_at_least_one_minute() {
        // 1 page x 50s = 50s, which rounds to ~1 min rather than 0.
        assert_eq!(minutes_left(1, 50), 1);
    }

    #[test]
    fn rounds_to_the_nearest_minute() {
        assert_eq!(minutes_left(2, 50), 2, "100s -> 2 min");
        assert_eq!(minutes_left(10, 50), 8, "500s -> ~8 min");
        assert_eq!(
            minutes_left(12, DEFAULT_SECS_PER_PAGE as i32),
            10,
            "600s -> 10 min"
        );
    }

    #[test]
    fn non_positive_rate_is_zero_not_a_panic() {
        assert_eq!(minutes_left(100, 0), 0);
        assert_eq!(minutes_left(100, -5), 0);
    }

    #[test]
    fn a_pathological_page_count_saturates_rather_than_overflowing() {
        assert_eq!(minutes_left(i32::MAX, i32::MAX), i32::MAX);
    }
}
