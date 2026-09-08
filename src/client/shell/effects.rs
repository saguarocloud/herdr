//! Pure, width-stable color math for static status-line chrome.
//!
//! Every function here is a pure function of `(position, palette)` — the
//! spatial per-character gradients used by the status line. They recolor
//! cells, they never add or remove them, so hit geometry is unaffected. All
//! interpolation requires true-color (`Color::Rgb`) endpoints; ANSI-indexed
//! themes degrade to the static base color. Upstream v0.8.0 removed the
//! animation tick (`perf: replace agent spinners with static status marks`),
//! so the fork's status line is static: no pulse/shimmer, only these
//! position-based gradients remain.

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
