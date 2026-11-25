//! The port's global header, on every screen: a message at the top left for
//! a few seconds (hidden otherwise), and with it, at the top right, the icon
//! of the control scheme in use.
//!
//! - At start: the icon alone, until a few seconds after the first input.
//!
//! Everything fades in and out; an icon flashes for its first seconds.
//! - Switching between keyboard / mouse and a controller: "Control scheme
//!   changed." and the new scheme's icon.
//! - A controller unplugged: "Control disconnected".
//!
//! The scheme is whatever sent the last input (input.rs calls `used`).

use crate::gfx::{label_width, Ui};
use std::sync::Mutex;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Scheme {
    Keyboard,
    Controller,
}

/// A message fades in over FADE_IN, stays until SHOW (seconds), then fades
/// out over FADE. An icon flashes for FLASH seconds after it appears.
const FADE_IN: f64 = 0.3;
const SHOW: f64 = 2.5;
const FADE: f64 = 0.5;
const FLASH: f64 = 2.0;
const FLASH_HZ: f64 = 2.5;
/// Text size (the game's font: cell height) and the header's margin, landscape points.
const TEXT: f32 = 13.0;
const MARGIN: f32 = 4.0;
/// The game's menu teal, and its brighter edge.
const TEAL: [f32; 3] = [0.33, 0.78, 0.68];
const TEAL_HI: [f32; 3] = [0.62, 0.95, 0.86];

struct State {
    /// None until the first input (the icon then follows the controllers).
    scheme: Option<Scheme>,
    /// The message and when it was shown.
    msg: Option<(String, f64)>,
    /// When the first input came (the start icon fades after it).
    first_input: Option<f64>,
    /// When the header was first drawn (the start icon fades in from then).
    start: Option<f64>,
}

static STATE: Mutex<State> = Mutex::new(State { scheme: None, msg: None, first_input: None, start: None });

/// Input came from `s` at time `now`.
pub fn used(s: Scheme, now: f64) {
    let mut st = STATE.lock().unwrap();
    if st.scheme.is_some_and(|old| old != s) {
        st.msg = Some(("Control scheme changed.".to_string(), now));
        println!("control scheme: {s:?}");
    }
    st.scheme = Some(s);
    st.first_input.get_or_insert(now);
}

/// A controller was unplugged; `left` are still connected.
pub fn disconnected(left: usize, now: f64) {
    let mut st = STATE.lock().unwrap();
    st.msg = Some(("Control disconnected".to_string(), now));
    if left == 0 && st.scheme == Some(Scheme::Controller) {
        st.scheme = Some(Scheme::Keyboard);
    }
}

/// The scheme in use (before any input: a controller if one is connected).
pub fn scheme(pads: usize) -> Scheme {
    STATE.lock().unwrap().scheme.unwrap_or(if pads > 0 { Scheme::Controller } else { Scheme::Keyboard })
}

fn mix(c: [f32; 3], a: f32) -> [f32; 4] {
    [c[0], c[1], c[2], a]
}

/// Opacity of something appearing at `since`: fading in.
fn fade_in(now: f64, since: f64) -> f32 {
    ((now - since) / FADE_IN).clamp(0.0, 1.0) as f32
}

/// Opacity of something that goes `SHOW` seconds after `since`: fading out.
fn fade_out(now: f64, since: f64) -> f32 {
    (1.0 - (now - since - SHOW) / FADE).clamp(0.0, 1.0) as f32
}

/// The header to draw now (empty when there is nothing to show).
pub fn frame(now: f64, pads: usize) -> Vec<Ui> {
    let mut st = STATE.lock().unwrap();
    let start = *st.start.get_or_insert(now);
    let scheme = st.scheme.unwrap_or(if pads > 0 { Scheme::Controller } else { Scheme::Keyboard });
    let msg = st.msg.as_ref().map(|(text, since)| (text, *since, fade_in(now, *since) * fade_out(now, *since))).filter(|m| m.2 > 0.0);
    // The icon: with a message, and at start until after the first input.
    let start_a = fade_in(now, start) * st.first_input.map_or(1.0, |t| fade_out(now, t));
    let msg_a = msg.map_or(0.0, |m| m.2);
    let icon_a = msg_a.max(start_a);
    // The icon (re)appears with each message: it flashes from then.
    let shown_since = msg.map_or(start, |m| m.1.max(start));
    let mut ui = Vec::new();
    if let Some((text, _, a)) = msg {
        // A see-through plate with a teal underline, like the game's bars.
        let w = label_width(text, TEXT) + 12.0;
        ui.push(Ui::Rect([MARGIN, MARGIN, w, 15.0], [0.02, 0.07, 0.07, 0.5 * a]));
        ui.push(Ui::Rect([MARGIN, MARGIN + 14.0, w, 1.0], mix(TEAL_HI, a)));
        ui.push(Ui::Label((MARGIN + 6.0, MARGIN + 7.5), text.clone(), TEXT, [0.93, 0.93, 0.89, a]));
    }
    if icon_a > 0.0 {
        // Flashing for its first seconds: the icon pulses, the plate stays.
        let t = now - shown_since;
        let pulse = if t < FLASH { (0.625 + 0.375 * (t * FLASH_HZ * std::f64::consts::TAU).cos()) as f32 } else { 1.0 };
        let (iw, ih) = (44.0, 30.0);
        let ix = 480.0 - MARGIN - iw;
        ui.push(Ui::Rect([ix, MARGIN, iw, ih], [0.02, 0.07, 0.07, 0.5 * icon_a]));
        ui.push(Ui::Rect([ix, MARGIN + ih - 1.0, iw, 1.0], mix(TEAL_HI, icon_a)));
        ui.extend(icon(scheme, (ix + iw / 2.0, MARGIN + ih / 2.0 - 0.5), icon_a * pulse));
    }
    ui
}

/// The controller's outline and the band above it (bumpers and the guide
/// button's part), in the reference picture's units (980 x 980).
const PAD_BODY: [(f32, f32); 40] = [
    (165.0, 285.0), (230.0, 262.0), (290.0, 246.0), (318.0, 252.0), (342.0, 268.0), (372.0, 300.0), (398.0, 326.0),
    (430.0, 338.0), (550.0, 338.0), (582.0, 326.0), (608.0, 300.0), (638.0, 268.0), (662.0, 252.0), (690.0, 246.0),
    (750.0, 262.0), (815.0, 285.0), (848.0, 318.0), (878.0, 410.0), (902.0, 510.0), (915.0, 610.0), (912.0, 705.0),
    (888.0, 778.0), (842.0, 802.0), (800.0, 785.0), (745.0, 722.0), (695.0, 665.0), (660.0, 650.0), (320.0, 650.0),
    (285.0, 665.0), (235.0, 722.0), (180.0, 785.0), (138.0, 802.0), (92.0, 778.0), (68.0, 705.0), (65.0, 610.0),
    (78.0, 510.0), (102.0, 410.0), (132.0, 318.0), (148.0, 300.0), (157.0, 291.0),
];
const PAD_BAND: [(f32, f32); 20] = [
    (182.0, 250.0), (205.0, 220.0), (255.0, 200.0), (305.0, 186.0), (338.0, 188.0), (378.0, 210.0), (602.0, 210.0),
    (642.0, 188.0), (675.0, 186.0), (725.0, 200.0), (775.0, 220.0), (798.0, 250.0), (745.0, 236.0), (690.0, 228.0),
    (650.0, 238.0), (590.0, 304.0), (550.0, 316.0), (430.0, 316.0), (390.0, 304.0), (330.0, 238.0),
];

/// The scheme's icon, centred at `c` (about 32 x 23 points), after the usual
/// controller and keyboard pictograms: one teal shape (a gradient from the
/// bright edge teal to a deeper one) with its details cut out in dark teal.
fn icon(s: Scheme, c: (f32, f32), a: f32) -> Vec<Ui> {
    let rgba = |c: [f32; 3]| [c[0], c[1], c[2], a];
    let light = rgba(TEAL_HI);
    let deep = rgba([TEAL[0] * 0.7, TEAL[1] * 0.7, TEAL[2] * 0.7]);
    let hole = rgba([0.03, 0.14, 0.13]);
    let mut v = Vec::new();
    match s {
        Scheme::Controller => {
            // Reference units -> points.
            let (k, ox, oy) = (32.0 / 850.0, 490.0, 495.0);
            let pt = |(x, y): (f32, f32)| (c.0 + (x - ox) * k, c.1 + (y - oy) * k);
            let shade = |y: f32| {
                let t = ((y - 185.0) / 617.0).clamp(0.0, 1.0);
                let m = |i: usize| light[i] + (deep[i] - light[i]) * t;
                [m(0), m(1), m(2), a]
            };
            v.push(Ui::Poly(PAD_BAND.iter().map(|&q| pt(q)).collect(), shade(186.0), shade(316.0)));
            v.push(Ui::Poly(PAD_BODY.iter().map(|&q| pt(q)).collect(), shade(246.0), shade(802.0)));
            v.push(Ui::Circle(pt((490.0, 262.0)), 30.0 * k, hole));
            // Two crosses (D-pad and left stick, as in the pictogram).
            for (x, y) in [(255.0, 395.0), (378.0, 532.0)] {
                let (cx, cy) = pt((x, y));
                v.push(Ui::RoundRect([cx - 66.0 * k, cy - 22.0 * k, 132.0 * k, 44.0 * k], 10.0 * k, hole, hole));
                v.push(Ui::RoundRect([cx - 22.0 * k, cy - 66.0 * k, 44.0 * k, 132.0 * k], 10.0 * k, hole, hole));
            }
            for x in [423.0, 555.0] {
                v.push(Ui::Circle(pt((x, 395.0)), 18.0 * k, hole));
            }
            for (x, y) in [(723.0, 335.0), (665.0, 395.0), (783.0, 395.0), (723.0, 455.0)] {
                v.push(Ui::Circle(pt((x, y)), 26.0 * k, hole));
            }
            // The right stick: a ring.
            v.push(Ui::Circle(pt((602.0, 528.0)), 62.0 * k, hole));
            v.push(Ui::Circle(pt((602.0, 528.0)), 47.0 * k, shade(528.0)));
        }
        Scheme::Keyboard => {
            // Reference units (1920 x 1920) -> points.
            let (k, ox, oy) = (32.0 / 1484.0, 960.0, 959.0);
            let pt = |(x, y): (f32, f32)| (c.0 + (x - ox) * k, c.1 + (y - oy) * k);
            let (x0, y0) = pt((218.0, 585.0));
            v.push(Ui::RoundRect([x0, y0, 1484.0 * k, 748.0 * k], 100.0 * k, light, deep));
            let key = |v: &mut Vec<Ui>, x: f32, y: f32, w: f32| {
                let (kx, ky) = pt((x, y));
                v.push(Ui::RoundRect([kx, ky, w * k, 97.0 * k], 16.0 * k, hole, hole));
            };
            for row in 0..3 {
                for col in 0..9 {
                    key(&mut v, 322.0 + col as f32 * 147.5, 690.0 + row as f32 * 148.0, 97.0);
                }
            }
            for col in [0, 1, 7, 8] {
                key(&mut v, 322.0 + col as f32 * 147.5, 1133.0, 97.0);
            }
            key(&mut v, 617.0, 1133.0, 686.0);
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_and_scheme() {
        // At start: the icon alone (no message), fading in, until after the
        // first input.
        let text = |ui: &[Ui]| ui.iter().any(|u| matches!(u, Ui::Label(..)));
        assert!(frame(0.0, 0).is_empty());
        let start_icon = frame(0.5, 0);
        assert!(!start_icon.is_empty() && !text(&start_icon));
        used(Scheme::Keyboard, 1.0);
        assert!(!text(&frame(1.2, 0)));
        assert!(frame(1.0 + SHOW + FADE + 0.1, 0).is_empty());
        // A switch shows the message and the icon for a while.
        used(Scheme::Controller, 10.0);
        assert!(text(&frame(10.5, 1)));
        assert!(frame(10.0 + SHOW + FADE + 0.1, 1).is_empty());
        assert_eq!(scheme(1), Scheme::Controller);
        // Unplugging the last controller goes back to the keyboard.
        disconnected(0, 20.0);
        assert_eq!(scheme(0), Scheme::Keyboard);
        assert!(!frame(20.5, 0).is_empty());
    }

    #[test]
    fn controller_outline_triangulates() {
        for pts in [&PAD_BODY[..], &PAD_BAND[..]] {
            assert_eq!(crate::gfx::triangulate(pts).len(), pts.len() - 2);
        }
    }
}
