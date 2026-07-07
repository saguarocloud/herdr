//! Pure, width-stable color effects for animated UI chrome.
//!
//! Every function here is a pure function of `(spinner_tick, palette)`, so
//! callers on the render path stay deterministic and hit geometry can never
//! change with animation — effects recolor cells, they never add or remove
//! them. All interpolation requires true-color (`Color::Rgb`) endpoints;
//! ANSI-indexed themes degrade to the static base color instead of animating.
//! [`wave`] is 0 at tick 0, so every effect renders exactly its static base
//! color on the first frame (and in tests, which pin `spinner_tick = 0`).

use ratatui::{
    style::{Color, Style},
    text::Span,
};

fn rgb_of(color: Color) -> Option<(u8, u8, u8)> {
    match color {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    }
}

/// True when `color` can participate in interpolation.
pub(super) fn is_rgb(color: Color) -> bool {
    rgb_of(color).is_some()
}

/// Linear blend from `from` to `to` at `t` in `[0, 1]`. Returns `from`
/// unchanged when either endpoint is not `Color::Rgb`.
pub(super) fn lerp_color(from: Color, to: Color, t: f32) -> Color {
    let (Some((r0, g0, b0)), Some((r1, g1, b1))) = (rgb_of(from), rgb_of(to)) else {
        return from;
    };
    let t = t.clamp(0.0, 1.0);
    let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
    Color::Rgb(mix(r0, r1), mix(g0, g1), mix(b0, b1))
}

/// Smooth cosine wave over `period` ticks: 0.0 at the start of each cycle,
/// 1.0 at the midpoint, and back. Being exactly 0 at tick 0 keeps the first
/// frame identical to the static rendering.
pub(super) fn wave(tick: u32, period: u32) -> f32 {
    let period = period.max(2);
    let phase = (tick % period) as f32 / period as f32;
    0.5 - 0.5 * (phase * std::f32::consts::TAU).cos()
}

/// Breathe `base` toward `toward`, at most `depth` of the way there.
pub(super) fn pulse_color(tick: u32, period: u32, base: Color, toward: Color, depth: f32) -> Color {
    lerp_color(base, toward, wave(tick, period) * depth.clamp(0.0, 1.0))
}

/// Breathe `base` toward darkness. Unlike a pulse toward a surface color this
/// only needs `base` itself to be RGB, so it works when the theme's surfaces
/// are `Reset`/indexed (e.g. the `terminal` theme with custom RGB accents).
pub(super) fn pulse_dim(tick: u32, period: u32, base: Color, depth: f32) -> Color {
    pulse_color(tick, period, base, Color::Rgb(0, 0, 0), depth)
}

/// Piecewise-linear blend across `stops` at `t` in `[0, 1]`. With two stops
/// this is `lerp_color`; more stops split the range evenly. Returns the first
/// stop unchanged when any stop is not `Color::Rgb`.
pub(super) fn lerp_stops(stops: &[Color], t: f32) -> Color {
    match stops {
        [] => Color::Reset,
        [only] => *only,
        _ => {
            if stops.iter().any(|stop| !is_rgb(*stop)) {
                return stops[0];
            }
            let t = t.clamp(0.0, 1.0) * (stops.len() - 1) as f32;
            let idx = (t.floor() as usize).min(stops.len() - 2);
            lerp_color(stops[idx], stops[idx + 1], t - idx as f32)
        }
    }
}

/// Render `text` with a per-character foreground fade across `stops` on top
/// of `base` (which keeps its bg and modifiers). Falls back to a single span
/// in the first stop when any stop is not RGB.
pub(super) fn gradient_spans(text: &str, stops: &[Color], base: Style) -> Vec<Span<'static>> {
    if text.is_empty() {
        return Vec::new();
    }
    let Some(&first) = stops.first() else {
        return vec![Span::styled(text.to_string(), base)];
    };
    if stops.len() < 2 || stops.iter().any(|stop| !is_rgb(*stop)) {
        return vec![Span::styled(text.to_string(), base.fg(first))];
    }
    let chars: Vec<char> = text.chars().collect();
    let denom = chars.len().saturating_sub(1).max(1) as f32;
    chars
        .into_iter()
        .enumerate()
        .map(|(i, c)| {
            let t = i as f32 / denom;
            Span::styled(c.to_string(), base.fg(lerp_stops(stops, t)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lerp_color_blends_rgb_endpoints() {
        let from = Color::Rgb(0, 0, 0);
        let to = Color::Rgb(100, 200, 50);
        assert_eq!(lerp_color(from, to, 0.0), from);
        assert_eq!(lerp_color(from, to, 1.0), to);
        assert_eq!(lerp_color(from, to, 0.5), Color::Rgb(50, 100, 25));
        // Out-of-range t clamps instead of overshooting.
        assert_eq!(lerp_color(from, to, 2.0), to);
    }

    #[test]
    fn lerp_color_falls_back_on_indexed_colors() {
        assert_eq!(lerp_color(Color::Red, Color::Rgb(0, 0, 0), 0.5), Color::Red);
        assert_eq!(
            lerp_color(Color::Rgb(10, 10, 10), Color::Reset, 0.5),
            Color::Rgb(10, 10, 10)
        );
    }

    #[test]
    fn wave_is_zero_at_cycle_start_and_peaks_mid_cycle() {
        assert_eq!(wave(0, 60), 0.0);
        assert_eq!(wave(60, 60), 0.0, "wraps back to the base color");
        assert!((wave(30, 60) - 1.0).abs() < 1e-5);
        assert!(wave(15, 60) > 0.4 && wave(15, 60) < 0.6);
    }

    #[test]
    fn pulse_color_is_base_at_tick_zero() {
        let base = Color::Rgb(200, 40, 40);
        let toward = Color::Rgb(20, 20, 30);
        assert_eq!(pulse_color(0, 72, base, toward, 0.65), base);
        assert_ne!(pulse_color(36, 72, base, toward, 0.65), base);
    }

    #[test]
    fn pulse_depth_limits_the_swing() {
        let base = Color::Rgb(100, 100, 100);
        let toward = Color::Rgb(0, 0, 0);
        // Peak of the wave at half depth lands halfway.
        assert_eq!(
            pulse_color(36, 72, base, toward, 0.5),
            Color::Rgb(50, 50, 50)
        );
    }

    #[test]
    fn lerp_stops_splits_the_range_across_stops() {
        let stops = [
            Color::Rgb(0, 0, 0),
            Color::Rgb(100, 0, 0),
            Color::Rgb(100, 0, 200),
        ];
        assert_eq!(lerp_stops(&stops, 0.0), stops[0]);
        assert_eq!(lerp_stops(&stops, 0.5), stops[1]);
        assert_eq!(lerp_stops(&stops, 1.0), stops[2]);
        assert_eq!(lerp_stops(&stops, 0.25), Color::Rgb(50, 0, 0));
        assert_eq!(lerp_stops(&stops, 0.75), Color::Rgb(100, 0, 100));
    }

    #[test]
    fn lerp_stops_falls_back_on_indexed_stops() {
        let stops = [Color::Rgb(0, 0, 0), Color::Cyan];
        assert_eq!(lerp_stops(&stops, 0.5), Color::Rgb(0, 0, 0));
        assert_eq!(lerp_stops(&[Color::Red], 0.5), Color::Red);
        assert_eq!(lerp_stops(&[], 0.5), Color::Reset);
    }

    #[test]
    fn gradient_spans_fade_per_character() {
        let spans = gradient_spans(
            "abc",
            &[Color::Rgb(0, 0, 0), Color::Rgb(200, 0, 0)],
            Style::default(),
        );
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].style.fg, Some(Color::Rgb(0, 0, 0)));
        assert_eq!(spans[1].style.fg, Some(Color::Rgb(100, 0, 0)));
        assert_eq!(spans[2].style.fg, Some(Color::Rgb(200, 0, 0)));
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "abc");
    }

    #[test]
    fn gradient_spans_visit_every_stop() {
        let stops = [
            Color::Rgb(0, 0, 0),
            Color::Rgb(100, 0, 0),
            Color::Rgb(100, 0, 200),
        ];
        let spans = gradient_spans("abcde", &stops, Style::default());
        assert_eq!(spans.len(), 5);
        assert_eq!(spans[0].style.fg, Some(stops[0]));
        assert_eq!(spans[2].style.fg, Some(stops[1]));
        assert_eq!(spans[4].style.fg, Some(stops[2]));
    }

    #[test]
    fn gradient_spans_fall_back_to_single_span_without_rgb() {
        let spans = gradient_spans("abc", &[Color::Cyan, Color::Rgb(0, 0, 0)], Style::default());
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].style.fg, Some(Color::Cyan));
        assert!(gradient_spans("", &[Color::Cyan, Color::Cyan], Style::default()).is_empty());
    }

    #[test]
    fn gradient_single_char_uses_start_color() {
        let spans = gradient_spans(
            "x",
            &[Color::Rgb(1, 2, 3), Color::Rgb(9, 9, 9)],
            Style::default(),
        );
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].style.fg, Some(Color::Rgb(1, 2, 3)));
    }
}
