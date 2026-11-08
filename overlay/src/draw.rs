use crate::config::Config;
use crate::voice::Member;
use smithay_client_toolkit::shell::wlr_layer::Anchor;
use std::f32::consts::PI;
use std::time::Duration;
use tiny_skia::{
    BlendMode, Color, FillRule, FilterQuality, LineCap, Mask, Paint, PathBuilder, Pixmap,
    PixmapPaint, Rect, Stroke, Transform,
};

#[derive(Debug, PartialEq)]
pub struct Layout {
    pub width: u32,
    pub height: u32,
    pub slots: Vec<(u32, u32)>,
}

pub fn layout(c: &Config, n: usize) -> Layout {
    let (a, s, b) = (c.avatar_size, c.icon_spacing, c.border_width);
    let (pad_along, pad_across) = if c.horizontal {
        (c.horz_edge_padding, c.vert_edge_padding)
    } else {
        (c.vert_edge_padding, c.horz_edge_padding)
    };
    let n = n as u32;
    let step = a + s;
    let (along, start) = match c.topalign {
        0 => (pad_along + n.saturating_sub(1) * step + a + b, pad_along),
        1 => (n * step + 2 * b, b),
        _ => (n * step + pad_along + b, b),
    };
    let across = pad_across + a + b;
    let pos = if c.rightalign { b } else { pad_across };
    let slots = (0..n).map(|i| (pos, start + i * step));
    if c.horizontal {
        Layout {
            width: along,
            height: across,
            slots: slots.map(|(y, x)| (x, y)).collect(),
        }
    } else {
        Layout {
            width: across,
            height: along,
            slots: slots.collect(),
        }
    }
}

pub fn anchor(c: &Config) -> Anchor {
    let (near, far, start, end) = if c.horizontal {
        (Anchor::TOP, Anchor::BOTTOM, Anchor::LEFT, Anchor::RIGHT)
    } else {
        (Anchor::LEFT, Anchor::RIGHT, Anchor::TOP, Anchor::BOTTOM)
    };
    let edge = if c.rightalign { far } else { near };
    match c.topalign {
        0 => edge | start,
        1 => edge,
        _ => edge | end,
    }
}

pub fn opacity(c: &Config, idle: Duration, speaking: bool) -> f32 {
    let wait = Duration::from_secs(c.inactive_time.into());
    if !c.fade_out_inactive || speaking || idle < wait {
        return 1.0;
    }
    let (t, total) = ((idle - wait).as_secs_f32(), c.inactive_fade_time as f32);
    let l = c.fade_out_limit;
    if t >= total {
        l
    } else {
        l + (1.0 - l) * (1.0 - t / total)
    }
}

pub fn next_tick(c: &Config, idle: Duration, speaking: bool) -> Option<Duration> {
    let wait = Duration::from_secs(c.inactive_time.into());
    let total = Duration::from_secs(c.inactive_fade_time.into());
    if !c.fade_out_inactive || speaking || idle >= wait + total {
        return None;
    }
    if idle < wait {
        return Some(wait - idle);
    }
    let steps = ((1.0 - c.fade_out_limit) * 255.0).ceil().max(1.0);
    Some(total.div_f32(steps).max(Duration::from_millis(50)))
}

fn col(c: [f32; 4]) -> Color {
    Color::from_rgba(c[0], c[1], c[2], c[3]).expect("colours are validated in config")
}

fn paint(c: Color) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(c);
    p
}

fn arc(pb: &mut PathBuilder, cx: f32, cy: f32, r: f32, a0: f32, mut a1: f32, new_subpath: bool) {
    while a1 < a0 {
        a1 += 2.0 * PI;
    }
    let pt = |a: f32| (cx + r * a.cos(), cy + r * a.sin());
    let (x, y) = pt(a0);
    if new_subpath {
        pb.move_to(x, y)
    } else {
        pb.line_to(x, y)
    }
    let n = ((a1 - a0) / (PI / 2.0)).ceil().max(1.0) as usize;
    let d = (a1 - a0) / n as f32;
    let k = 4.0 / 3.0 * (d / 4.0).tan() * r;
    for i in 0..n {
        let (s, e) = (a0 + d * i as f32, a0 + d * (i + 1) as f32);
        let (p0, p3) = (pt(s), pt(e));
        pb.cubic_to(
            p0.0 - k * s.sin(),
            p0.1 + k * s.cos(),
            p3.0 + k * e.sin(),
            p3.1 - k * e.cos(),
            p3.0,
            p3.1,
        );
    }
}

fn placeholder(pix: &mut Pixmap, id: &str, alpha: f32, t: Transform) {
    const PALETTE: [[u8; 3]; 6] = [
        [0x58, 0x65, 0xF2],
        [0x75, 0x7E, 0x8A],
        [0x3B, 0xA5, 0x5C],
        [0xFA, 0xA6, 0x1A],
        [0xED, 0x42, 0x45],
        [0xEB, 0x45, 0x9E],
    ];
    let [r, g, b] = PALETTE[(id.parse::<u64>().unwrap_or(0) >> 22) as usize % PALETTE.len()];
    let a = (alpha * 255.0).round() as u8;
    pix.fill_rect(
        Rect::from_xywh(0.0, 0.0, 1.0, 1.0).unwrap(),
        &paint(Color::from_rgba8(r, g, b, a)),
        t,
        None,
    );
    let mut pb = PathBuilder::new();
    pb.push_circle(0.5, 0.36, 0.17);
    arc(&mut pb, 0.5, 0.86, 0.29, PI, 0.0, true);
    pb.close();
    let white = paint(Color::from_rgba8(255, 255, 255, a));
    pix.fill_path(&pb.finish().unwrap(), &white, FillRule::Winding, t, None);
}

fn line(pb: &mut PathBuilder, x0: f32, y0: f32, x1: f32, y1: f32) {
    pb.move_to(x0, y0);
    pb.line_to(x1, y1);
}

fn mute_icon(pix: &mut Pixmap, c: &Config, deaf: bool, t: Transform) {
    let unit = Rect::from_xywh(0.0, 0.0, 1.0, 1.0).unwrap();
    let mut dim = paint(col(c.mt_bg_col));
    dim.blend_mode = BlendMode::SourceAtop;
    pix.fill_rect(unit, &dim, t, None);

    let mut pb = PathBuilder::new();
    pb.push_rect(unit);
    arc(&mut pb, 0.9, 0.1, 0.05, 1.25 * PI, 2.25 * PI, true);
    arc(&mut pb, 0.1, 0.9, 0.05, 0.25 * PI, 1.25 * PI, false);
    pb.close();
    let mut clip = Mask::new(pix.width(), pix.height()).unwrap();
    clip.fill_path(&pb.finish().unwrap(), FillRule::EvenOdd, true, t);

    let fg = paint(col(c.mt_col));
    let stroke = |pix: &mut Pixmap, pb: PathBuilder, width: f32| {
        let s = Stroke {
            width,
            ..Stroke::default()
        };
        pix.stroke_path(&pb.finish().unwrap(), &fg, &s, t, Some(&clip));
    };
    if deaf {
        let mut band = PathBuilder::new();
        arc(&mut band, 0.5, 0.5, 0.2, PI, 0.0, true);
        stroke(pix, band, 0.1);
        let mut left = PathBuilder::new();
        arc(&mut left, 0.28, 0.65, 0.075, 1.5 * PI, 0.5 * PI, true);
        line(&mut left, 0.3, 0.5, 0.3, 0.75);
        stroke(pix, left, 0.1);
        let mut right = PathBuilder::new();
        arc(&mut right, 0.72, 0.65, 0.075, 0.5 * PI, 1.5 * PI, true);
        line(&mut right, 0.7, 0.5, 0.7, 0.75);
        stroke(pix, right, 0.1);
    } else {
        let mut mic = PathBuilder::new();
        arc(&mut mic, 0.5, 0.3, 0.1, PI, 2.0 * PI, true);
        arc(&mut mic, 0.5, 0.5, 0.1, 0.0, PI, false);
        mic.close();
        pix.fill_path(
            &mic.finish().unwrap(),
            &fg,
            FillRule::Winding,
            t,
            Some(&clip),
        );
        let mut stand = PathBuilder::new();
        arc(&mut stand, 0.5, 0.5, 0.15, 0.0, PI, true);
        stroke(pix, stand, 0.05);
        let mut base = PathBuilder::new();
        line(&mut base, 0.5, 0.65, 0.5, 0.75);
        line(&mut base, 0.35, 0.75, 0.65, 0.75);
        stroke(pix, base, 0.05);
    }

    let mut strike = PathBuilder::new();
    line(&mut strike, 0.7, 0.3, 0.3, 0.7);
    let s = Stroke {
        width: 0.07,
        line_cap: LineCap::Round,
        ..Stroke::default()
    };
    pix.stroke_path(&strike.finish().unwrap(), &fg, &s, t, None);
}

pub fn render<'a>(
    c: &Config,
    lay: &Layout,
    members: &[Member],
    scale: u32,
    avatar: impl Fn(&Member) -> Option<&'a Pixmap>,
) -> Pixmap {
    let mut pix = Pixmap::new(lay.width * scale, lay.height * scale).expect("non-empty layout");
    let base = Transform::from_scale(scale as f32, scale as f32);
    let a = c.avatar_size as f32;
    for (m, &(x, y)) in members.iter().zip(&lay.slots) {
        let (x, y) = (x as f32, y as f32);
        let unit = base.pre_translate(x, y).pre_scale(a, a);
        match avatar(m) {
            Some(img) => {
                let p = PixmapPaint {
                    opacity: c.icon_transparency,
                    quality: FilterQuality::Bicubic,
                    ..Default::default()
                };
                let t = base
                    .pre_translate(x, y)
                    .pre_scale(a / img.width() as f32, a / img.height() as f32);
                pix.draw_pixmap(0, 0, img.as_ref(), &p, t, None);
            }
            None => placeholder(&mut pix, &m.id, c.icon_transparency, unit),
        }
        let b = c.border_width as f32;
        if m.speaking && !m.muted() && !m.deafened() && b > 0.0 {
            let r = Rect::from_xywh(x - b / 2.0, y - b / 2.0, a + b, a + b).unwrap();
            let s = Stroke {
                width: b,
                ..Stroke::default()
            };
            pix.stroke_path(
                &PathBuilder::from_rect(r),
                &paint(col(c.tk_col)),
                &s,
                base,
                None,
            );
        }
        if m.deafened() || m.muted() {
            mute_icon(&mut pix, c, m.deafened(), unit);
        }
    }
    pix
}

pub fn to_argb(rgba: &[u8], opacity: u8, out: &mut [u8]) {
    let o = opacity as u16;
    let m = |v: u8| ((v as u16 * o + 127) / 255) as u8;
    for (s, d) in rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(out.as_chunks_mut::<4>().0)
    {
        *d = [m(s[2]), m(s[1]), m(s[0]), m(s[3])];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(horizontal: bool, rightalign: bool, topalign: u8) -> Config {
        Config {
            horizontal,
            rightalign,
            topalign,
            ..Config::default()
        }
    }

    #[test]
    fn default_is_left_middle_column() {
        let c = Config::default();
        let l = layout(&c, 4);
        assert_eq!((l.width, l.height), (42, 156));
        assert_eq!(l.slots, [(10, 2), (10, 40), (10, 78), (10, 116)]);
        assert_eq!(anchor(&c), Anchor::LEFT);
        let surface_y = 40 + (1040 - l.height) / 2;
        assert_eq!(
            l.slots.iter().map(|s| surface_y + s.1).collect::<Vec<_>>(),
            [484, 522, 560, 598]
        );
    }

    #[test]
    fn column_top_bottom_right() {
        let l = layout(&cfg(false, false, 0), 2);
        assert_eq!(
            (l.width, l.height, l.slots.clone()),
            (42, 5 + 38 + 30 + 2, vec![(10, 5), (10, 43)])
        );
        assert_eq!(anchor(&cfg(false, false, 0)), Anchor::TOP | Anchor::LEFT);
        let l = layout(&cfg(false, true, 2), 2);
        assert_eq!(
            (l.width, l.height, l.slots.clone()),
            (42, 2 * 38 + 5 + 2, vec![(2, 2), (2, 40)])
        );
        assert_eq!(anchor(&cfg(false, true, 2)), Anchor::BOTTOM | Anchor::RIGHT);
    }

    #[test]
    fn row_top_centre_matches_spec_example() {
        let c = cfg(true, false, 1);
        let l = layout(&c, 4);
        assert_eq!((l.width, l.height), (156, 37));
        assert_eq!(l.slots, [(2, 5), (40, 5), (78, 5), (116, 5)]);
        assert_eq!(anchor(&c), Anchor::TOP);
        let surface_x = (1920 - l.width) / 2;
        assert_eq!(surface_x, 882);
        assert_eq!(surface_x + l.slots[0].0, 884);
    }

    #[test]
    fn row_edges_and_padding_spacing() {
        let c = Config {
            horizontal: true,
            rightalign: true,
            topalign: 0,
            horz_edge_padding: 7,
            icon_spacing: 3,
            ..Config::default()
        };
        let l = layout(&c, 3);
        assert_eq!((l.width, l.height), (7 + 2 * 33 + 30 + 2, 5 + 30 + 2));
        assert_eq!(l.slots, [(7, 2), (40, 2), (73, 2)]);
        assert_eq!(anchor(&c), Anchor::BOTTOM | Anchor::LEFT);
        assert_eq!(anchor(&cfg(true, false, 2)), Anchor::TOP | Anchor::RIGHT);
    }

    #[test]
    fn fade_timeline() {
        let c = Config::default();
        let s = Duration::from_secs_f32;
        assert_eq!(opacity(&c, s(9.9), false), 1.0);
        assert_eq!(opacity(&c, s(10.0), false), 1.0);
        assert!((opacity(&c, s(25.0), false) - 0.75).abs() < 1e-4);
        assert_eq!(opacity(&c, s(40.0), false), 0.5);
        assert_eq!(opacity(&c, s(400.0), false), 0.5);
        assert_eq!(opacity(&c, s(30.0), true), 1.0, "speaking = full opacity");
        let off = Config {
            fade_out_inactive: false,
            ..Config::default()
        };
        assert_eq!(opacity(&off, s(400.0), false), 1.0);
        let instant = Config {
            inactive_fade_time: 0,
            ..Config::default()
        };
        assert_eq!(opacity(&instant, s(10.0), false), 0.5);
    }

    #[test]
    fn fade_ticks_only_while_needed() {
        let c = Config::default();
        let s = Duration::from_secs_f32;
        assert_eq!(next_tick(&c, s(0.0), false), Some(s(10.0)));
        assert_eq!(next_tick(&c, s(4.0), false), Some(s(6.0)));
        let tick = next_tick(&c, s(12.0), false).unwrap();
        assert!(
            tick >= Duration::from_millis(50) && tick <= Duration::from_millis(300),
            "{tick:?}"
        );
        assert!(
            30.0 / tick.as_secs_f32() <= 130.0,
            "at most ~128 frames per fade"
        );
        assert_eq!(next_tick(&c, s(40.0), false), None, "dimmed: sleep");
        assert_eq!(
            next_tick(&c, s(3.0), true),
            None,
            "speaking: sleep until the next state line"
        );
        let off = Config {
            fade_out_inactive: false,
            ..Config::default()
        };
        assert_eq!(next_tick(&off, s(0.0), false), None);
    }

    #[test]
    fn render_border_and_icons_land_where_expected() {
        let c = Config::default();
        let line = r#"{"v":1,"self_id":"1","channel":{},"members":[
            {"id":"1","name":"a","avatar":null,"speaking":true},
            {"id":"2","name":"b","avatar":null,"self_mute":true},
            {"id":"3","name":"c","avatar":null,"deaf":true,"speaking":true}]}"#;
        let members =
            crate::voice::visible(crate::voice::parse_line(line.as_bytes()).unwrap(), true);
        let lay = layout(&c, 3);
        let pix = render(&c, &lay, &members, 1, |_| None);
        let px = |x: u32, y: u32| pix.pixel(x, y).unwrap().demultiply();
        let green = px(9, 20);
        assert_eq!((green.red(), green.blue(), green.alpha()), (0, 0, 255));
        assert!((178..=179).contains(&green.green()), "{green:?}");
        assert_eq!(px(9, 58).alpha(), 0, "muted speaker has no border");
        assert_eq!(px(9, 96).alpha(), 0, "deafened speaker has no border");
        let strike = px(10 + 14, 40 + 15);
        assert!(
            strike.red() > 140 && strike.green() < 10 && strike.blue() < 10,
            "{strike:?}"
        );
        let dim = px(10 + 3, 40 + 3);
        assert!(dim.blue() > 100 && dim.blue() < 130, "{dim:?}");
        assert_eq!(px(0, 0).alpha(), 0, "outside avatars stays transparent");
    }

    #[test]
    fn argb_conversion_swaps_and_fades() {
        let mut out = [0u8; 4];
        to_argb(&[10, 20, 200, 255], 255, &mut out);
        assert_eq!(out, [200, 20, 10, 255]);
        to_argb(&[10, 20, 200, 255], 128, &mut out);
        assert_eq!(out, [100, 10, 5, 128]);
    }
}
