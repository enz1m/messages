use iced::Color;
fn luminance(c: Color) -> f32 {
    let linear = |v: f32| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(c.r) + 0.7152 * linear(c.g) + 0.0722 * linear(c.b)
}
fn contrast(a: Color, b: Color) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}
pub(super) fn selection_distinct(fill: Color, accent: Color) -> Color {
    if contrast(fill, accent) >= 3.0 {
        return fill;
    }
    let target = if contrast(Color::BLACK, accent) > contrast(Color::WHITE, accent) {
        Color::BLACK
    } else {
        Color::WHITE
    };
    for step in 1..=32 {
        let t = step as f32 / 32.0;
        let candidate = Color {
            r: fill.r + (target.r - fill.r) * t,
            g: fill.g + (target.g - fill.g) * t,
            b: fill.b + (target.b - fill.b) * t,
            a: 1.0,
        };
        if contrast(candidate, accent) >= 3.0 {
            return candidate;
        }
    }
    target
}
