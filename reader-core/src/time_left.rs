pub fn estimate_time_left(
    current_page: u32,
    total_pages: Option<u32>,
    avg_seconds_per_page: f64,
) -> Option<u32> {
    let total = total_pages.filter(|&t| t > 0)?;
    let pages_left = total.saturating_sub(current_page);
    let seconds = pages_left as f64 * avg_seconds_per_page.max(0.0);
    Some((seconds / 60.0).ceil() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_when_total_unknown() {
        assert_eq!(estimate_time_left(1, None, 30.0), None);
    }

    #[test]
    fn none_when_total_zero() {
        assert_eq!(estimate_time_left(0, Some(0), 30.0), None);
    }

    #[test]
    fn basic_minutes() {
        assert_eq!(estimate_time_left(10, Some(20), 60.0), Some(10));
    }

    #[test]
    fn rounds_up() {
        assert_eq!(estimate_time_left(9, Some(10), 30.0), Some(1));
    }

    #[test]
    fn finished_is_zero() {
        assert_eq!(estimate_time_left(20, Some(20), 60.0), Some(0));
    }

    #[test]
    fn past_end_is_zero() {
        assert_eq!(estimate_time_left(25, Some(20), 60.0), Some(0));
    }

    #[test]
    fn zero_pace_is_zero() {
        assert_eq!(estimate_time_left(0, Some(10), 0.0), Some(0));
    }
}
