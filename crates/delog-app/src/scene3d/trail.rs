use std::ops::Range;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TrailMode {
    #[default]
    ToPlayhead,
    VisibleWindow,
    Full,
}

impl TrailMode {
    pub fn next(self) -> Self {
        match self {
            Self::ToPlayhead => Self::VisibleWindow,
            Self::VisibleWindow => Self::Full,
            Self::Full => Self::ToPlayhead,
        }
    }
}

pub fn trail_range(
    times_us: &[i64],
    mode: TrailMode,
    playhead_us: Option<i64>,
    window_us: Option<(i64, i64)>,
) -> Range<u32> {
    let len = times_us.len();
    let range = match (mode, playhead_us, window_us) {
        (TrailMode::ToPlayhead, Some(playhead), _) => {
            0..times_us.partition_point(|&ts| ts <= playhead)
        }
        (TrailMode::VisibleWindow, _, Some((min_us, max_us))) => {
            let first = times_us.partition_point(|&ts| ts < min_us);
            let last = times_us.partition_point(|&ts| ts <= max_us);
            first.saturating_sub(1)..(last + 1).min(len)
        }
        _ => 0..len,
    };
    if range.len() < 2 {
        return 0..0;
    }
    range.start as u32..range.end as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMES: [i64; 6] = [0, 10, 20, 30, 40, 50];

    #[test]
    fn full_mode_draws_every_point() {
        assert_eq!(
            trail_range(&TIMES, TrailMode::Full, Some(25), Some((15, 35))),
            0..6
        );
    }

    #[test]
    fn to_playhead_mode_stops_at_the_playhead() {
        assert_eq!(
            trail_range(&TIMES, TrailMode::ToPlayhead, Some(25), None),
            0..3
        );
    }

    #[test]
    fn to_playhead_mode_includes_a_sample_exactly_on_the_playhead() {
        assert_eq!(
            trail_range(&TIMES, TrailMode::ToPlayhead, Some(20), None),
            0..3
        );
    }

    #[test]
    fn to_playhead_mode_without_a_playhead_draws_every_point() {
        assert_eq!(trail_range(&TIMES, TrailMode::ToPlayhead, None, None), 0..6);
    }

    #[test]
    fn visible_window_keeps_the_segments_crossing_both_edges() {
        assert_eq!(
            trail_range(&TIMES, TrailMode::VisibleWindow, Some(25), Some((15, 35))),
            1..5
        );
    }

    #[test]
    fn visible_window_ignores_the_playhead() {
        assert_eq!(
            trail_range(&TIMES, TrailMode::VisibleWindow, Some(0), Some((15, 35))),
            1..5
        );
    }

    #[test]
    fn visible_window_spanning_the_whole_trail_draws_every_point() {
        assert_eq!(
            trail_range(&TIMES, TrailMode::VisibleWindow, None, Some((-10, 100))),
            0..6
        );
    }

    #[test]
    fn visible_window_inside_one_gap_keeps_the_spanning_segment() {
        assert_eq!(
            trail_range(&[0, 100], TrailMode::VisibleWindow, None, Some((40, 60))),
            0..2
        );
    }

    #[test]
    fn visible_window_before_the_trail_draws_nothing() {
        assert_eq!(
            trail_range(&[100, 110], TrailMode::VisibleWindow, None, Some((0, 50))),
            0..0
        );
    }

    #[test]
    fn visible_window_after_the_trail_draws_nothing() {
        assert_eq!(
            trail_range(&[0, 10], TrailMode::VisibleWindow, None, Some((50, 90))),
            0..0
        );
    }

    #[test]
    fn visible_window_without_a_view_draws_every_point() {
        assert_eq!(
            trail_range(&TIMES, TrailMode::VisibleWindow, Some(25), None),
            0..6
        );
    }

    #[test]
    fn a_single_point_is_never_drawable() {
        assert_eq!(trail_range(&[7], TrailMode::Full, None, None), 0..0);
    }

    #[test]
    fn an_empty_trail_draws_nothing() {
        assert_eq!(trail_range(&[], TrailMode::Full, None, None), 0..0);
    }

    #[test]
    fn modes_cycle_from_playhead_through_window_to_full() {
        assert_eq!(TrailMode::ToPlayhead.next(), TrailMode::VisibleWindow);
        assert_eq!(TrailMode::VisibleWindow.next(), TrailMode::Full);
        assert_eq!(TrailMode::Full.next(), TrailMode::ToPlayhead);
    }

    #[test]
    fn the_default_mode_clips_to_the_playhead() {
        assert_eq!(TrailMode::default(), TrailMode::ToPlayhead);
    }
}
