//! Mahjong Soul result pages: see the OK button before pressing it.
//!
//! Every page the client puts up when a hand or a game ends — the win and
//! its yaku, the score change, the final standings, the rank change, the
//! rewards — is closed by the same gold OK button in the bottom-right
//! corner. A press only registers once that button is on screen: presses
//! during a page's entrance animation are dropped, and the pages take
//! different times to come up (a hand page ~7–8 s after the win, the next
//! one ~5 s after it is closed; a game's three pages likewise). Presses on
//! a timer therefore miss whenever a page is slower than the timer, and a
//! game-end page nobody closes stays up for good — the lobby presses after
//! it then land on the result page and the queue never starts.
//!
//! So the presses here wait for the button. A small clip of the screen
//! where it sits is captured (`Page.captureScreenshot`, which reads the
//! composited frame, so it works on the WebGL canvas) and its share of
//! gold pixels counted: ~0.63 with the button up, ≤0.05 on the lobby and
//! the table. When the button has been in sight for a reaction time, it is
//! pressed; the next page's button is waited for the same way.

use crate::autoplay::cdp_input::{
    capture_region, dispatch_click, evaluate_canvas_rect, snapshot, Pixels,
};
use crate::autoplay::context::{AutoplayContext, CanvasRect};
use crate::config::MajsoulAutoplayConfig;
use anyhow::Result;
use chromiumoxide::page::Page;
use rand::Rng;
use std::time::{Duration, Instant};
use tracing::{debug, info};

/// Where the OK button's face sits, 16:9 units: `((x0, x1), (y0, y1))`.
/// Inside the button's edges (it spans ~13.5–15.6 × 7.95–8.5), so a page
/// whose button is a few pixels off still fills the box.
pub const OK_BOX: ((f64, f64), (f64, f64)) = ((13.75, 15.35), (8.02, 8.48));

/// Share of gold pixels in [`OK_BOX`] from which the button counts as up.
/// Measured: 0.63 on all three game-end pages; 0.05 on the lobby, 0.00 on
/// the table. A button still fading in sits in between and is not pressed.
pub const GOLD_SEEN: f64 = 0.30;

/// Captured width of the box, pixels — enough to count colours, small
/// enough to cost a few milliseconds.
const SAMPLE_WIDTH: f64 = 120.0;

/// Pause between looks.
const LOOK_EVERY: (u64, u64) = (220, 380);
/// After a press, the soonest the same button is pressed again if it is
/// still up (the press was dropped).
const REPRESS_GAP: Duration = Duration::from_millis(1600);
/// Failed captures in a row before sight counts as lost.
const LOST_AFTER: u32 = 6;
/// Click scatter around the OK button, 16:9 units.
const JITTER: (f64, f64) = (0.07, 0.04);

/// The button's gold: bright, warm, little blue.
pub fn is_gold(r: u8, g: u8, b: u8) -> bool {
    let (r, g, b) = (i32::from(r), i32::from(g), i32::from(b));
    r >= 190 && g >= 140 && b <= 120 && r - b >= 100
}

/// Share of gold pixels in an image, `0.0..=1.0`.
pub fn gold_share(px: &Pixels) -> f64 {
    let n = px.rgb.len() / 3;
    if n == 0 {
        return 0.0;
    }
    let gold = px
        .rgb
        .chunks_exact(3)
        .filter(|p| is_gold(p[0], p[1], p[2]))
        .count();
    gold as f64 / n as f64
}

/// The CSS-pixel clip of [`OK_BOX`] on a canvas.
pub fn ok_clip(rect: &CanvasRect) -> (f64, f64, f64, f64) {
    let ((x0, x1), (y0, y1)) = OK_BOX;
    let (px0, py0) = rect.pixel(x0, y0);
    let (px1, py1) = rect.pixel(x1, y1);
    (px0, py0, (px1 - px0).max(1.0), (py1 - py0).max(1.0))
}

/// Gold share of the OK button's spot right now.
pub async fn ok_gold(page: &Page, rect: &CanvasRect) -> Result<f64> {
    let (x, y, w, h) = ok_clip(rect);
    let scale = (SAMPLE_WIDTH / w).clamp(0.05, 1.0);
    let px = capture_region(page, x, y, w, h, scale).await?;
    Ok(gold_share(&px))
}

/// Gold share of the OK spot on the bound game page, if there is one.
pub async fn look(ctx: &AutoplayContext) -> Result<f64> {
    let page = ctx.page.read().await.as_ref().cloned();
    let page = page.ok_or_else(|| anyhow::anyhow!("no game page"))?;
    let rect = evaluate_canvas_rect(&page).await?;
    if rect.width <= 0.0 || rect.height <= 0.0 {
        anyhow::bail!("the game canvas has no size");
    }
    ok_gold(&page, &rect).await
}

/// One press at a 16:9 point on the bound page, scattered a little, with
/// the in-game hover and hold.
pub async fn press(ctx: &AutoplayContext, ms: &MajsoulAutoplayConfig, at: [f64; 2]) {
    let page = ctx.page.read().await.as_ref().cloned();
    let Some(page) = page else { return };
    let rect = match evaluate_canvas_rect(&page).await {
        Ok(r) if r.width > 0.0 && r.height > 0.0 => r,
        Ok(_) => return,
        Err(e) => {
            debug!("autoplay: OK press: no canvas rect: {e:#}");
            return;
        }
    };
    let (jx, jy) = {
        let mut rng = rand::rng();
        (
            rng.random_range(-JITTER.0..=JITTER.0),
            rng.random_range(-JITTER.1..=JITTER.1),
        )
    };
    let (x, y) = rect.pixel(at[0] + jx, at[1] + jy);
    if !rect.contains(x, y) {
        return;
    }
    let ms = ms.draw(&mut rand::rng());
    if let Err(e) = dispatch_click(&page, x, y, ms.hover_delay_ms.ms(), ms.click_hold_ms.ms()).await
    {
        debug!("autoplay: OK press failed: {e:#}");
    }
}

/// What a watch over the result pages has seen so far; handed to the
/// caller's stop test on every look.
#[derive(Debug, Clone, Copy)]
pub struct PagesSoFar {
    /// Presses made.
    pub presses: u32,
    /// Since the button was last in sight (`None`: never).
    pub since_seen: Option<Duration>,
    /// Since the last press (`None`: none yet).
    pub since_press: Option<Duration>,
    pub elapsed: Duration,
}

/// How a watch ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagesEnd {
    /// The caller's test said the pages are over.
    Done { presses: u32 },
    /// `limit` passed first.
    TimedOut { presses: u32 },
    /// Captures kept failing (no page, window minimised, …): the caller
    /// falls back to pressing blind.
    SightLost { presses: u32 },
}

/// Watch the OK spot and press the button each time it has been up for a
/// reaction time (`ok_reaction_ms`, drawn per page), until `done` says the
/// pages are over or `limit` passes.
///
/// `diagnose`: if no OK has come into sight this long into the watch, save
/// one screenshot of the page under this tag (`cdp_input::snapshot`) — the
/// picture of a page the button test does not recognise.
pub async fn press_through_pages(
    ctx: &AutoplayContext,
    ms: &MajsoulAutoplayConfig,
    at: [f64; 2],
    limit: Duration,
    diagnose: Option<(Duration, &str)>,
    mut done: impl FnMut(&PagesSoFar) -> bool,
) -> PagesEnd {
    let start = Instant::now();
    let mut diagnosed = diagnose.is_none();
    let mut presses = 0u32;
    let mut failures = 0u32;
    let mut up_since: Option<Instant> = None;
    let mut last_seen: Option<Instant> = None;
    let mut last_press: Option<Instant> = None;
    let mut reaction = reaction_time(ms);
    loop {
        let now = Instant::now();
        let so_far = PagesSoFar {
            presses,
            since_seen: last_seen.map(|t| now - t),
            since_press: last_press.map(|t| now - t),
            elapsed: now - start,
        };
        if done(&so_far) {
            return PagesEnd::Done { presses };
        }
        if so_far.elapsed > limit {
            return PagesEnd::TimedOut { presses };
        }
        if let (false, Some((after, tag))) = (diagnosed, diagnose) {
            if last_seen.is_none() && so_far.elapsed >= after {
                diagnosed = true;
                let page = ctx.page.read().await.as_ref().cloned();
                if let Some(page) = page {
                    if let Some(path) = snapshot(&page, tag).await {
                        info!(
                            "autoplay: no result OK in sight after {}s; screenshot: {}",
                            after.as_secs(),
                            path.display()
                        );
                    }
                }
            }
        }
        match look(ctx).await {
            Ok(gold) => {
                failures = 0;
                if gold >= GOLD_SEEN {
                    let now = Instant::now();
                    last_seen = Some(now);
                    let since = *up_since.get_or_insert(now);
                    let rested = last_press.is_none_or(|t| now - t >= REPRESS_GAP);
                    if now - since >= reaction && rested {
                        debug!("autoplay: result OK in sight (gold {gold:.2}); pressing");
                        press(ctx, ms, at).await;
                        presses += 1;
                        last_press = Some(Instant::now());
                        up_since = None;
                        reaction = reaction_time(ms);
                    }
                } else {
                    up_since = None;
                }
            }
            Err(e) => {
                failures += 1;
                debug!("autoplay: result OK sight failed ({failures}): {e:#}");
                if failures >= LOST_AFTER {
                    return PagesEnd::SightLost { presses };
                }
            }
        }
        let pause = rand::rng().random_range(LOOK_EVERY.0..=LOOK_EVERY.1);
        tokio::time::sleep(Duration::from_millis(pause)).await;
    }
}

fn reaction_time(ms: &MajsoulAutoplayConfig) -> Duration {
    Duration::from_millis(u64::from(ms.ok_reaction_ms.draw(&mut rand::rng()).ms()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(colours: &[(u8, u8, u8)]) -> Pixels {
        Pixels {
            width: colours.len() as u32,
            height: 1,
            rgb: colours.iter().flat_map(|&(r, g, b)| [r, g, b]).collect(),
        }
    }

    #[test]
    fn button_gold_is_gold() {
        // Face, highlight and the darker lower rim of the button.
        assert!(is_gold(250, 205, 70));
        assert!(is_gold(255, 230, 110));
        assert!(is_gold(214, 160, 40));
    }

    #[test]
    fn table_and_lobby_colours_are_not() {
        assert!(!is_gold(240, 240, 240), "white tile face");
        assert!(!is_gold(30, 90, 60), "table felt");
        assert!(!is_gold(20, 25, 45), "dark panel");
        assert!(!is_gold(230, 120, 140), "pink");
        assert!(!is_gold(200, 170, 150), "skin / beige: too much blue");
    }

    #[test]
    fn share_counts_gold_pixels() {
        let img = image(&[
            (250, 205, 70),
            (250, 205, 70),
            (30, 90, 60),
            (240, 240, 240),
        ]);
        assert!((gold_share(&img) - 0.5).abs() < 1e-9);
        assert_eq!(gold_share(&image(&[])), 0.0);
    }

    #[test]
    fn clip_tracks_the_canvas() {
        let rect = CanvasRect {
            x: 0.0,
            y: 167.0,
            width: 1173.33,
            height: 660.0,
        };
        let (x, y, w, h) = ok_clip(&rect);
        // Inside the button measured on a 1173×660 canvas: x 991–1143,
        // y 749–792 (screen).
        assert!(x > 991.0 && x + w < 1143.0, "x {x} w {w}");
        assert!(y > 749.0 && y + h < 793.0, "y {y} h {h}");
    }

    #[test]
    fn png_round_trip() {
        // A 2×1 RGBA PNG: one gold pixel, one green.
        let mut bytes = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut bytes, 2, 1);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[250, 205, 70, 255, 30, 90, 60, 255])
                .unwrap();
        }
        let px = crate::autoplay::cdp_input::decode_png_rgb(&bytes).unwrap();
        assert_eq!((px.width, px.height), (2, 1));
        assert_eq!(px.rgb, vec![250, 205, 70, 30, 90, 60]);
        assert!((gold_share(&px) - 0.5).abs() < 1e-9);
    }
}
