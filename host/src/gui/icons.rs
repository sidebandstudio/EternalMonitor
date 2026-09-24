//! Line icons painted on a 16-point grid, so they stay crisp at any scale.

use std::f32::consts::PI;

use eframe::egui::{
    pos2, vec2, Color32, CornerRadius, Painter, Pos2, Rect, Shape, Stroke, StrokeKind,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Display,
    Activity,
    Sliders,
    Qr,
    Copy,
    Check,
    Refresh,
    Stop,
    Play,
    Plug,
    Speaker,
    Lock,
    Tablet,
    Warning,
    External,
    Logs,
    Download,
    Chevron,
}

/// Paint `icon` centred in `rect`.
pub fn paint(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let unit = rect.width().min(rect.height()) / 16.0;
    let origin = rect.center() - vec2(8.0, 8.0) * unit;
    let p = |x: f32, y: f32| pos2(origin.x + x * unit, origin.y + y * unit);
    let r = |x0: f32, y0: f32, x1: f32, y1: f32| Rect::from_min_max(p(x0, y0), p(x1, y1));
    let stroke = Stroke::new((1.45 * unit).max(1.1), color);
    let line = |points: Vec<Pos2>| painter.add(Shape::line(points, stroke));
    let outline = |rect: Rect, radius: f32| {
        painter.rect_stroke(rect, radius * unit, stroke, StrokeKind::Middle);
    };
    let arc = |cx: f32, cy: f32, radius: f32, from: f32, to: f32| -> Vec<Pos2> {
        let steps = 20;
        (0..=steps)
            .map(|i| {
                let a = (from + (to - from) * i as f32 / steps as f32) * PI / 180.0;
                p(cx + radius * a.cos(), cy + radius * a.sin())
            })
            .collect()
    };

    match icon {
        Icon::Display => {
            outline(r(1.5, 2.5, 14.5, 11.5), 1.8);
            line(vec![p(8.0, 11.5), p(8.0, 14.0)]);
            line(vec![p(5.0, 14.0), p(11.0, 14.0)]);
        }
        Icon::Activity => {
            line(vec![
                p(1.5, 8.5),
                p(4.5, 8.5),
                p(6.5, 3.0),
                p(9.5, 13.0),
                p(11.5, 8.5),
                p(14.5, 8.5),
            ]);
        }
        Icon::Sliders => {
            for (y, knob) in [(4.0, 10.5), (8.0, 5.5), (12.0, 9.5)] {
                line(vec![p(2.0, y), p(14.0, y)]);
                painter.circle_filled(p(knob, y), 2.1 * unit, color);
            }
        }
        Icon::Qr => {
            for (x, y) in [(1.8, 1.8), (9.7, 1.8), (1.8, 9.7)] {
                outline(r(x, y, x + 4.5, y + 4.5), 1.0);
                painter.rect_filled(
                    r(x + 1.6, y + 1.6, x + 2.9, y + 2.9),
                    CornerRadius::ZERO,
                    color,
                );
            }
            for (x, y) in [(9.7, 9.7), (12.6, 9.7), (12.6, 12.6)] {
                painter.rect_filled(r(x, y, x + 1.7, y + 1.7), CornerRadius::ZERO, color);
            }
        }
        Icon::Copy => {
            outline(r(5.5, 5.5, 14.0, 14.0), 1.8);
            line(vec![p(2.0, 10.5), p(2.0, 3.5), p(3.5, 2.0), p(10.5, 2.0)]);
        }
        Icon::Check => {
            line(vec![p(2.8, 8.4), p(6.4, 12.0), p(13.2, 4.2)]);
        }
        Icon::Refresh => {
            let points = arc(8.0, 8.0, 5.6, -30.0, 255.0);
            let end = points[0];
            line(points);
            // Arrow head at the start of the arc, pointing clockwise-back.
            line(vec![
                end + vec2(-3.4, 0.2) * unit,
                end,
                end + vec2(0.4, -3.4) * unit,
            ]);
        }
        Icon::Stop => {
            painter.rect_filled(
                r(3.8, 3.8, 12.2, 12.2),
                CornerRadius::same((1.8 * unit) as u8),
                color,
            );
        }
        Icon::Play => {
            painter.add(Shape::convex_polygon(
                vec![p(4.5, 2.8), p(13.0, 8.0), p(4.5, 13.2)],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Plug => {
            outline(r(4.0, 5.5, 12.0, 10.8), 2.0);
            line(vec![p(6.4, 2.0), p(6.4, 5.5)]);
            line(vec![p(9.6, 2.0), p(9.6, 5.5)]);
            line(vec![p(8.0, 10.8), p(8.0, 14.5)]);
        }
        Icon::Speaker => {
            painter.add(Shape::closed_line(
                vec![
                    p(1.8, 6.0),
                    p(4.6, 6.0),
                    p(8.2, 2.8),
                    p(8.2, 13.2),
                    p(4.6, 10.0),
                    p(1.8, 10.0),
                ],
                stroke,
            ));
            line(arc(8.2, 8.0, 3.2, -45.0, 45.0));
            line(arc(8.2, 8.0, 6.0, -48.0, 48.0));
        }
        Icon::Lock => {
            outline(r(3.0, 7.0, 13.0, 14.5), 2.0);
            let mut shackle = vec![p(5.0, 7.0)];
            shackle.extend(arc(8.0, 5.2, 3.0, 180.0, 360.0));
            shackle.push(p(11.0, 7.0));
            line(shackle);
            painter.circle_filled(p(8.0, 10.7), 1.1 * unit, color);
        }
        Icon::Tablet => {
            outline(r(3.2, 1.5, 12.8, 14.5), 2.0);
            line(vec![p(7.0, 12.2), p(9.0, 12.2)]);
        }
        Icon::Warning => {
            painter.add(Shape::closed_line(
                vec![p(8.0, 1.8), p(14.8, 13.8), p(1.2, 13.8)],
                stroke,
            ));
            line(vec![p(8.0, 6.0), p(8.0, 9.6)]);
            painter.circle_filled(p(8.0, 11.7), 1.0 * unit, color);
        }
        Icon::External => {
            line(vec![p(6.0, 10.0), p(13.0, 3.0)]);
            line(vec![p(8.2, 3.0), p(13.0, 3.0), p(13.0, 7.8)]);
            line(vec![
                p(11.0, 10.0),
                p(11.0, 13.0),
                p(3.0, 13.0),
                p(3.0, 5.0),
                p(6.0, 5.0),
            ]);
        }
        Icon::Logs => {
            outline(r(3.0, 1.5, 13.0, 14.5), 1.8);
            for y in [5.0, 8.0, 11.0] {
                line(vec![p(5.6, y), p(10.4, y)]);
            }
        }
        Icon::Chevron => {
            line(vec![p(4.0, 6.0), p(8.0, 10.0), p(12.0, 6.0)]);
        }
        Icon::Download => {
            line(vec![p(8.0, 2.0), p(8.0, 10.5)]);
            line(vec![p(4.5, 7.0), p(8.0, 10.5), p(11.5, 7.0)]);
            line(vec![p(2.5, 13.5), p(13.5, 13.5)]);
        }
    }
}
