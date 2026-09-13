//! How things move: easing curves, keyframed values and transitions
//! (SPEC.md sections 4.8 and 4.9).

use unbaked_core::recipe::{Animatable, Ease, Transition, TransitionKind};

/// Bisection steps when solving a Bézier curve for its parameter. 2⁻⁴⁰ is far
/// inside the spec's 10⁻⁶, and a fixed count keeps every run identical.
const SOLVE_STEPS: u32 = 40;

/// The eased progress `E(p)` for progress `p` in 0–1.
pub fn ease(curve: Ease, p: f64) -> f64 {
    match curve {
        Ease::Linear => p,
        Ease::Hold => {
            if p >= 1.0 {
                1.0
            } else {
                0.0
            }
        }
        Ease::Bezier([x1, y1, x2, y2]) => {
            if p <= 0.0 {
                return 0.0;
            }
            if p >= 1.0 {
                return 1.0;
            }
            let u = solve(x1, x2, p);
            cubic(y1, y2, u)
        }
    }
}

/// A cubic Bézier coordinate with end points 0 and 1 and control values `a`, `b`.
fn cubic(a: f64, b: f64, u: f64) -> f64 {
    let v = 1.0 - u;
    3.0 * v * v * u * a + 3.0 * v * u * u * b + u * u * u
}

/// The curve parameter `u` where the x coordinate equals `x`. With `x1` and `x2`
/// in 0–1 the x coordinate only rises, so bisection always finds it.
fn solve(x1: f64, x2: f64, x: f64) -> f64 {
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..SOLVE_STEPS {
        let mid = 0.5 * (lo + hi);
        if cubic(x1, x2, mid) < x {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

/// The value of an animatable number at local time `t_ms`.
pub fn value_at(value: &Animatable, t_ms: f64) -> f64 {
    let keys = match value {
        Animatable::Constant(v) => return *v,
        Animatable::Keys(keys) => keys,
    };
    let (Some(first), Some(last)) = (keys.first(), keys.last()) else {
        return 0.0;
    };
    if t_ms <= first.t_ms as f64 {
        return first.v;
    }
    if t_ms >= last.t_ms as f64 {
        return last.v;
    }
    // The first key after t; keys strictly increase, so its predecessor is at or before t.
    let next = keys.partition_point(|k| (k.t_ms as f64) <= t_ms);
    let (a, b) = (&keys[next - 1], &keys[next]);
    let p = (t_ms - a.t_ms as f64) / (b.t_ms - a.t_ms) as f64;
    a.v + (b.v - a.v) * ease(a.ease, p)
}

/// What a layer's `in` and `out` transitions do at one moment: multipliers and
/// offsets applied on top of the layer's own values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransitionState {
    pub opacity: f64,
    /// Multiplies both `scale_x` and `scale_y`.
    pub scale: f64,
    pub dx: f64,
    pub dy: f64,
}

impl TransitionState {
    pub const NONE: TransitionState = TransitionState {
        opacity: 1.0,
        scale: 1.0,
        dx: 0.0,
        dy: 0.0,
    };
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    In,
    Out,
}

/// Combines a layer's transitions at local time `t_ms`. `length_ms` is the
/// layer's visible length (`out` has no effect without one); `width` and
/// `height` are the output canvas size. Where `in` and `out` overlap, both apply.
pub fn transitions(
    transition_in: Option<&Transition>,
    transition_out: Option<&Transition>,
    t_ms: f64,
    length_ms: Option<u64>,
    width: f64,
    height: f64,
) -> TransitionState {
    let mut state = TransitionState::NONE;
    if let Some(t) = transition_in {
        let p = ease(t.ease, (t_ms / t.duration_ms as f64).clamp(0.0, 1.0));
        apply(&mut state, t.kind, Side::In, p, width, height);
    }
    if let (Some(t), Some(length)) = (transition_out, length_ms) {
        let remaining = length as f64 - t_ms;
        let p = ease(t.ease, (remaining / t.duration_ms as f64).clamp(0.0, 1.0));
        apply(&mut state, t.kind, Side::Out, p, width, height);
    }
    state
}

/// One row of the table in section 4.9. The name says which way the layer moves.
fn apply(state: &mut TransitionState, kind: TransitionKind, side: Side, p: f64, w: f64, h: f64) {
    let q = 1.0 - p;
    // For `in`, the layer arrives from the side opposite its direction of travel.
    let sign = if side == Side::In { -1.0 } else { 1.0 };
    match kind {
        TransitionKind::Fade => state.opacity *= p,
        TransitionKind::Zoom => {
            state.opacity *= p;
            state.scale *= p;
        }
        TransitionKind::SlideLeft => state.dx -= sign * q * w,
        TransitionKind::SlideRight => state.dx += sign * q * w,
        TransitionKind::SlideUp => state.dy -= sign * q * h,
        TransitionKind::SlideDown => state.dy += sign * q * h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unbaked_core::recipe::Key;

    fn close(a: f64, b: f64, tolerance: f64) -> bool {
        (a - b).abs() <= tolerance
    }

    #[test]
    fn linear_and_hold() {
        assert_eq!(ease(Ease::Linear, 0.25), 0.25);
        assert_eq!(ease(Ease::Hold, 0.0), 0.0);
        assert_eq!(ease(Ease::Hold, 0.999), 0.0);
        assert_eq!(ease(Ease::Hold, 1.0), 1.0);
    }

    #[test]
    fn css_curves_match_their_reference_shape() {
        for curve in [Ease::EASE, Ease::EASE_IN, Ease::EASE_OUT, Ease::EASE_IN_OUT] {
            assert_eq!(ease(curve, 0.0), 0.0);
            assert_eq!(ease(curve, 1.0), 1.0);
            let mut previous = 0.0;
            for i in 1..=100 {
                let y = ease(curve, f64::from(i) / 100.0);
                assert!(y >= previous, "{curve:?} must not go backwards");
                previous = y;
            }
        }
        assert!(close(ease(Ease::EASE_IN_OUT, 0.5), 0.5, 1e-9));
        for i in 0..=20 {
            let x = f64::from(i) / 20.0;
            assert!(close(
                ease(Ease::EASE_OUT, x),
                1.0 - ease(Ease::EASE_IN, 1.0 - x),
                1e-9
            ));
        }
        // Reference values of CSS cubic-bezier() at 50%.
        assert!(
            close(ease(Ease::EASE, 0.5), 0.8024, 1e-3),
            "{}",
            ease(Ease::EASE, 0.5)
        );
        assert!(
            close(ease(Ease::EASE_IN, 0.5), 0.3154, 1e-3),
            "{}",
            ease(Ease::EASE_IN, 0.5)
        );
    }

    #[test]
    fn solved_parameter_is_within_spec_tolerance() {
        let (x1, x2) = (0.25, 0.25);
        for i in 1..100 {
            let x = f64::from(i) / 100.0;
            assert!(close(cubic(x1, x2, solve(x1, x2, x)), x, 1e-9));
        }
    }

    #[test]
    fn overshooting_curves_leave_0_to_1() {
        let back = Ease::Bezier([0.3, 2.0, 0.7, 2.0]);
        assert!((0..=100).any(|i| ease(back, f64::from(i) / 100.0) > 1.0));
    }

    fn key(t_ms: u64, v: f64, ease: Ease) -> Key {
        Key { t_ms, v, ease }
    }

    #[test]
    fn keyframes_follow_section_4_8() {
        // The example from SPEC.md section 4.8.
        let x = Animatable::Keys(vec![
            key(0, -400.0, Ease::EASE_OUT),
            key(600, 100.0, Ease::Linear),
            key(3000, 100.0, Ease::Hold),
            key(3001, 900.0, Ease::Linear),
        ]);
        assert_eq!(value_at(&x, -5.0), -400.0);
        assert_eq!(value_at(&x, 0.0), -400.0);
        assert!(
            value_at(&x, 300.0) > -150.0,
            "ease-out is past halfway at half time"
        );
        assert_eq!(value_at(&x, 600.0), 100.0);
        assert_eq!(value_at(&x, 1800.0), 100.0);
        assert_eq!(value_at(&x, 3000.5), 100.0);
        assert_eq!(value_at(&x, 3001.0), 900.0);
        assert_eq!(value_at(&x, 99_999.0), 900.0);

        let line = Animatable::Keys(vec![
            key(100, 0.0, Ease::Linear),
            key(200, 10.0, Ease::Linear),
        ]);
        assert_eq!(value_at(&line, 150.0), 5.0);
        assert_eq!(value_at(&line, 125.5), 2.55);
        assert_eq!(value_at(&Animatable::Constant(7.0), 1e9), 7.0);
        assert_eq!(
            value_at(&Animatable::Keys(vec![key(50, 3.0, Ease::Hold)]), 0.0),
            3.0
        );
    }

    fn transition(kind: TransitionKind, duration_ms: u64) -> Transition {
        Transition {
            kind,
            duration_ms,
            ease: Ease::Linear,
        }
    }

    #[test]
    fn slides_move_the_way_their_name_says() {
        let (w, h) = (1920.0, 1080.0);
        let state = |kind, side_in: bool, t| {
            let t_in = transition(kind, 400);
            let t_out = transition(kind, 400);
            if side_in {
                transitions(Some(&t_in), None, t, Some(1000), w, h)
            } else {
                transitions(None, Some(&t_out), t, Some(1000), w, h)
            }
        };
        // In: enters from the opposite side, arriving at its own position.
        assert_eq!(state(TransitionKind::SlideLeft, true, 0.0).dx, w);
        assert_eq!(state(TransitionKind::SlideLeft, true, 400.0).dx, 0.0);
        assert_eq!(state(TransitionKind::SlideRight, true, 0.0).dx, -w);
        assert_eq!(state(TransitionKind::SlideUp, true, 0.0).dy, h);
        assert_eq!(state(TransitionKind::SlideDown, true, 200.0).dy, -h / 2.0);
        // Out: leaves in the named direction, fully gone at the end.
        assert_eq!(state(TransitionKind::SlideLeft, false, 1000.0).dx, -w);
        assert_eq!(state(TransitionKind::SlideLeft, false, 600.0).dx, 0.0);
        assert_eq!(state(TransitionKind::SlideRight, false, 1000.0).dx, w);
        assert_eq!(state(TransitionKind::SlideUp, false, 1000.0).dy, -h);
        assert_eq!(state(TransitionKind::SlideDown, false, 800.0).dy, h / 2.0);
    }

    #[test]
    fn fades_and_zooms_scale_by_progress_and_overlap() {
        let fade_in = transition(TransitionKind::Fade, 300);
        let fade_out = transition(TransitionKind::Fade, 300);
        let one_side = transitions(Some(&fade_in), None, 150.0, None, 1.0, 1.0);
        assert_eq!(one_side.opacity, 0.5);
        assert_eq!(one_side.scale, 1.0);

        // A 400 ms layer: at 200 ms both fades are two thirds done and both apply.
        let both = transitions(Some(&fade_in), Some(&fade_out), 200.0, Some(400), 1.0, 1.0);
        assert!(close(
            both.opacity,
            (200.0f64 / 300.0) * (200.0 / 300.0),
            1e-12
        ));

        let zoom = transition(TransitionKind::Zoom, 100);
        let zooming = transitions(Some(&zoom), None, 25.0, None, 1.0, 1.0);
        assert_eq!((zooming.opacity, zooming.scale), (0.25, 0.25));

        // Without a length, `out` does nothing.
        assert_eq!(
            transitions(None, Some(&fade_out), 0.0, None, 1.0, 1.0),
            TransitionState::NONE
        );
    }
}
