//! When things happen: frame times, layer spans and visibility (SPEC.md section 5.2).
//!
//! Visibility is decided with whole-number arithmetic, so every renderer agrees
//! on exactly which frames show a layer.

use unbaked_core::recipe::Fps;

/// The moment being rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Moment {
    /// Output frame `n` of a video at `fps`.
    Frame { n: u64, fps: Fps },
    /// The single moment an `image` shows, `output.at_ms`.
    AtMs(u64),
}

/// How many frames a video of `duration_ms` has: `ceil(duration_ms × num / (1000 × den))`.
pub fn frame_count(duration_ms: u64, fps: Fps) -> u64 {
    let top = u128::from(duration_ms) * u128::from(fps.num);
    let bottom = 1000 * u128::from(fps.den);
    u64::try_from(top.div_ceil(bottom)).unwrap_or(u64::MAX)
}

impl Moment {
    /// The moment in milliseconds. Fractional for frames that do not land on a whole millisecond.
    pub fn ms(self) -> f64 {
        match self {
            Moment::Frame { n, fps } => {
                (1000 * u128::from(n) * u128::from(fps.den)) as f64 / fps.num as f64
            }
            Moment::AtMs(ms) => ms as f64,
        }
    }

    /// Compares this moment with a whole millisecond `ms` exactly:
    /// the sign of `moment − ms`.
    fn cmp_ms(self, ms: u64) -> std::cmp::Ordering {
        match self {
            Moment::Frame { n, fps } => {
                let lhs = 1000 * u128::from(n) * u128::from(fps.den);
                let rhs = u128::from(ms) * u128::from(fps.num);
                lhs.cmp(&rhs)
            }
            Moment::AtMs(at) => at.cmp(&ms),
        }
    }
}

/// A layer's or clip's absolute visible time, in whole milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Absolute start `S`.
    pub start: u64,
    /// Absolute end `E`, exclusive. `None` when it never ends.
    pub end: Option<u64>,
}

impl Span {
    /// The span of the whole scene: from 0 to `output.duration_ms`, or without end.
    pub fn scene(duration_ms: Option<u64>) -> Span {
        Span {
            start: 0,
            end: duration_ms,
        }
    }

    /// The span of a layer inside this one (a group's child, or a mask layer inside
    /// the masked layer). Its times are measured from this span's start, and it
    /// never outlasts this span.
    pub fn child(self, start_ms: u64, end_ms: Option<u64>) -> Span {
        let start = self.start.saturating_add(start_ms);
        let own_end = end_ms.map(|e| self.start.saturating_add(e));
        let end = match (own_end, self.end) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        Span { start, end }
    }

    /// Visible when `S ≤ moment < E`, compared exactly.
    pub fn visible_at(self, moment: Moment) -> bool {
        use std::cmp::Ordering::*;
        let after_start = moment.cmp_ms(self.start) != Less;
        let before_end = self.end.is_none_or(|end| moment.cmp_ms(end) == Less);
        after_start && before_end
    }

    /// Local time at `moment`: milliseconds since this span's start. May be fractional.
    pub fn local_ms(self, moment: Moment) -> f64 {
        match moment {
            Moment::Frame { n, fps } => {
                let num = i128::from(fps.num);
                let scaled =
                    1000 * i128::from(n) * i128::from(fps.den) - i128::from(self.start) * num;
                scaled as f64 / num as f64
            }
            Moment::AtMs(ms) => ms as f64 - self.start as f64,
        }
    }

    /// Local time at `moment` plus `offset_ms`, as the exact fraction
    /// `num / den` milliseconds. A video layer's source time is its local
    /// time plus `trim_start_ms`.
    pub fn local_fraction(self, moment: Moment, offset_ms: u64) -> (i128, i128) {
        let shift = i128::from(offset_ms) - i128::from(self.start);
        match moment {
            Moment::Frame { n, fps } => {
                let num = i128::from(fps.num);
                (
                    1000 * i128::from(n) * i128::from(fps.den) + shift * num,
                    num,
                )
            }
            Moment::AtMs(ms) => (i128::from(ms) + shift, 1),
        }
    }

    /// The visible length `E − S`, used by `out` transitions. `None` without an end.
    pub fn length_ms(self) -> Option<u64> {
        self.end.map(|end| end.saturating_sub(self.start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NTSC: Fps = Fps {
        num: 30000,
        den: 1001,
    };
    const THIRTY: Fps = Fps { num: 30, den: 1 };

    fn frame(n: u64, fps: Fps) -> Moment {
        Moment::Frame { n, fps }
    }

    #[test]
    fn frame_count_rounds_up() {
        assert_eq!(frame_count(1000, THIRTY), 30);
        assert_eq!(frame_count(1001, THIRTY), 31);
        // 8000 × 30000 / 1001000 = 239.76…
        assert_eq!(frame_count(8000, NTSC), 240);
        assert_eq!(frame_count(1, THIRTY), 1);
    }

    #[test]
    fn frame_times() {
        assert_eq!(frame(0, THIRTY).ms(), 0.0);
        assert_eq!(frame(3, THIRTY).ms(), 100.0);
        assert!((frame(30, NTSC).ms() - 1001.0).abs() < 1e-9);
    }

    #[test]
    fn start_is_inclusive_and_end_exclusive_on_exact_frames() {
        let layer = Span::scene(Some(8000)).child(1000, Some(2000));
        assert!(!layer.visible_at(frame(29, THIRTY)));
        assert!(layer.visible_at(frame(30, THIRTY)));
        assert!(layer.visible_at(frame(59, THIRTY)));
        assert!(!layer.visible_at(frame(60, THIRTY)));
    }

    #[test]
    fn frames_between_milliseconds_compare_exactly() {
        // At 30000/1001 fps, frame 29 is at 967.63 ms and frame 30 at 1001 ms.
        let layer = Span::scene(None).child(1000, None);
        assert!(!layer.visible_at(frame(29, NTSC)));
        assert!(layer.visible_at(frame(30, NTSC)));
        assert!((layer.local_ms(frame(30, NTSC)) - 1.0).abs() < 1e-12);
        // Exactly 1 ms in, plus a 250 ms trim: 30·1001·1000/30000 − 1000 + 250.
        assert_eq!(
            layer.local_fraction(frame(30, NTSC), 250),
            (1000 * 30 * 1001 - 750 * 30000, 30000)
        );
        assert_eq!(
            Span::scene(None)
                .child(1000, None)
                .local_fraction(Moment::AtMs(400), 0),
            (-600, 1)
        );
    }

    #[test]
    fn children_are_measured_from_and_capped_by_their_parent() {
        let group = Span::scene(Some(8000)).child(1000, Some(3000));
        let child = group.child(500, Some(5000));
        assert_eq!(
            child,
            Span {
                start: 1500,
                end: Some(3000)
            }
        );
        assert_eq!(child.length_ms(), Some(1500));
        let open = Span::scene(None).child(0, None).child(250, None);
        assert_eq!(open.end, None);
        assert_eq!(open.length_ms(), None);
    }

    #[test]
    fn images_use_their_exact_moment() {
        let layer = Span::scene(None).child(1000, Some(2000));
        assert!(!layer.visible_at(Moment::AtMs(999)));
        assert!(layer.visible_at(Moment::AtMs(1000)));
        assert!(!layer.visible_at(Moment::AtMs(2000)));
        assert_eq!(layer.local_ms(Moment::AtMs(1500)), 500.0);
    }

    #[test]
    fn a_child_starting_after_its_parent_ends_is_never_visible() {
        let child = Span::scene(Some(1000)).child(1200, None);
        assert_eq!(child.length_ms(), Some(0));
        assert!(!child.visible_at(Moment::AtMs(1200)));
    }
}
