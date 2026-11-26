//! Mahjong Soul result pages: see the OK button before pressing it.
//!
//! Every page the client puts up when a hand or a game ends is closed by
//! an OK button in the bottom-right corner, at the same spot: on a hand's
//! pages (the win and its yaku, the score change, a draw) a navy button
//! with a countdown, "(3)確認"; on a game's pages (the standings, the
//! rewards) a gold one. A press only registers once the button is on
//! screen — presses during a page's entrance animation are dropped — and
//! the pages take different times to come up (a hand page ~8–10 s after
//! the win, a game's first page 20 s and more after its end). Presses on a
//! timer therefore miss whenever a page is slower than the timer, and a
//! game-end page nobody closes stays up for good: the lobby presses after
//! it land on the result page and the queue never starts.
//!
//! So the presses here wait for the button. The viewport is captured
//! (`Page.captureScreenshot`, which reads the composited frame, so it works
//! on the WebGL canvas) and the button's spot counted for its colours:
//! gold ~0.63 with the gold button up, navy ~0.35 with the navy one, both
//! 0.00 on the table and ≤0.05 on the lobby. When a button has been in
//! sight for a reaction time, it is pressed; the next page's button is
//! waited for the same way.

use crate::autoplay::cdp_input::{
    capture_viewport, dispatch_click, evaluate_canvas_rect, snapshot, Pixels,
};
use crate::autoplay::context::{AutoplayContext, CanvasRect};
use crate::config::MajsoulAutoplayConfig;
use anyhow::{anyhow, Result};
use rand::Rng;
use std::time::{Duration, Instant};
use tracing::{debug, info};

/// Where the OK buttons' faces sit, 16:9 units: `((x0, x1), (y0, y1))`.
/// The gold button spans ~13.5–15.6 × 7.95–8.55, the navy one
/// ~13.9–15.3 × 8.1–8.5; the box lies (nearly) inside both.
pub const OK_BOX: ((f64, f64), (f64, f64)) = ((13.75, 15.35), (8.02, 8.48));

/// Share of gold pixels in [`OK_BOX`] from which the gold button counts as
/// up. Measured 0.63 on every game-end page with the button; 0.05 on the
/// lobby, 0.00 on the table. A button still fading in sits in between and
/// is not pressed yet.
pub const GOLD_SEEN: f64 = 0.30;
/// The same for the navy button's blue: measured 0.35–0.37 on hand-result
/// and draw pages with the button, 0.00 everywhere else.
pub const BLUE_SEEN: f64 = 0.20;

/// Pause between looks. A look is a full-viewport capture, so not too
/// often.
const LOOK_EVERY: (u64, u64) = (320, 520);
/// After a press, the soonest the same button is pressed again if it is
/// still up (the press was dropped).
const REPRESS_GAP: Duration = Duration::from_millis(1600);
/// Failed captures in a row before sight counts as lost.
const LOST_AFTER: u32 = 6;
/// Click scatter around the OK button, 16:9 units.
const JITTER: (f64, f64) = (0.07, 0.04);

/// The gold button's gold: bright, warm, little blue.
pub fn is_gold(r: u8, g: u8, b: u8) -> bool {
    let (r, g, b) = (i32::from(r), i32::from(g), i32::from(b));
    r >= 190 && g >= 140 && b <= 120 && r - b >= 100
}

/// The navy button's blue — far brighter and bluer than the table's navy
/// felt (~17, 28, 46).
pub fn is_button_blue(r: u8, g: u8, b: u8) -> bool {
    let (r, g, b) = (i32::from(r), i32::from(g), i32::from(b));
    b >= 110 && b - r >= 50 && b - g >= 25 && r <= 110
}

/// What the OK spot shows: shares of gold and of button blue.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct OkLook {
    pub gold: f64,
    pub blue: f64,
}

impl OkLook {
    /// The gold button (game-end pages) is up.
    pub fn gold_up(&self) -> bool {
        self.gold >= GOLD_SEEN
    }
    /// Either button is up.
    pub fn up(&self) -> bool {
        self.gold_up() || self.blue >= BLUE_SEEN
    }
}

/// Count an image's gold and button-blue pixels.
pub fn classify(px: &Pixels) -> OkLook {
    let (chunks, _) = px.rgb.as_chunks::<3>();
    if chunks.is_empty() {
        return OkLook::default();
    }
    let (mut gold, mut blue) = (0usize, 0usize);
    for &[r, g, b] in chunks {
        if is_gold(r, g, b) {
            gold += 1;
        } else if is_button_blue(r, g, b) {
            blue += 1;
        }
    }
    let n = chunks.len() as f64;
    OkLook {
        gold: gold as f64 / n,
        blue: blue as f64 / n,
    }
}

/// [`OK_BOX`] in the pixels of a viewport capture `size` = (width, height),
/// for a canvas at `rect` in a viewport of `inner` CSS pixels.
pub fn ok_box_px(rect: &CanvasRect, inner: (f64, f64), size: (u32, u32)) -> (u32, u32, u32, u32) {
    let ((x0, x1), (y0, y1)) = OK_BOX;
    let (sx, sy) = (
        f64::from(size.0) / inner.0.max(1.0),
        f64::from(size.1) / inner.1.max(1.0),
    );
    let (px0, py0) = rect.pixel(x0, y0);
    let (px1, py1) = rect.pixel(x1, y1);
    let px = |v: f64, s: f64| (v * s).round().max(0.0) as u32;
    (px(px0, sx), px(py0, sy), px(px1, sx), px(py1, sy))
}

/// Canvas rect and viewport size in one round trip.
const GEOMETRY_JS: &str = "(()=>{const c=document.getElementsByTagName('canvas')[0];\
if(!c)return null;const r=c.getBoundingClientRect();\
return [r.left,r.top,r.width,r.height,window.innerWidth,window.innerHeight]})()";

/// What the OK spot on the bound game page shows right now.
pub async fn look(ctx: &AutoplayContext) -> Result<OkLook> {
    let page = ctx.page.read().await.as_ref().cloned();
    let page = page.ok_or_else(|| anyhow!("no game page"))?;
    let geo: Option<[f64; 6]> = page
        .evaluate(GEOMETRY_JS)
        .await
        .map_err(|e| anyhow!("canvas geometry: {e}"))?
        .into_value()
        .map_err(|e| anyhow!("canvas geometry: {e}"))?;
    let [x, y, width, height, iw, ih] = geo.ok_or_else(|| anyhow!("no canvas"))?;
    if width <= 0.0 || height <= 0.0 {
        anyhow::bail!("the game canvas has no size");
    }
    let rect = CanvasRect {
        x,
        y,
        width,
        height,
    };
    let frame = capture_viewport(&page).await?;
    let (x0, y0, x1, y1) = ok_box_px(&rect, (iw, ih), (frame.width, frame.height));
    Ok(classify(&frame.crop(x0, y0, x1, y1)))
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
            Ok(seen) => {
                failures = 0;
                if seen.up() {
                    let now = Instant::now();
                    last_seen = Some(now);
                    let since = *up_since.get_or_insert(now);
                    let rested = last_press.is_none_or(|t| now - t >= REPRESS_GAP);
                    if now - since >= reaction && rested {
                        info!(
                            "autoplay: result OK in sight (gold {:.2}, blue {:.2}); pressing",
                            seen.gold, seen.blue
                        );
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
        // Face, highlight and the darker lower rim of the gold button.
        assert!(is_gold(250, 205, 70));
        assert!(is_gold(255, 230, 110));
        assert!(is_gold(214, 160, 40));
        assert!(!is_button_blue(250, 205, 70));
    }

    #[test]
    fn button_navy_is_blue() {
        // Sampled off the "(2)確認" button of a score-change page.
        assert!(is_button_blue(49, 68, 124));
        assert!(is_button_blue(52, 67, 124));
        assert!(!is_gold(49, 68, 124));
    }

    #[test]
    fn table_and_lobby_colours_are_neither() {
        for (r, g, b, what) in [
            (240, 240, 240, "white tile face"),
            (17, 28, 46, "navy felt"),
            (13, 20, 46, "navy felt, darker"),
            (30, 90, 60, "green felt"),
            (20, 25, 45, "dark panel"),
            (230, 120, 140, "pink"),
            (200, 170, 150, "beige: too much blue for gold"),
            (65, 62, 105, "dim violet: not blue enough"),
        ] {
            assert!(!is_gold(r, g, b), "{what}");
            assert!(!is_button_blue(r, g, b), "{what}");
        }
    }

    #[test]
    fn classify_counts_both_buttons() {
        let gold = image(&[
            (250, 205, 70),
            (250, 205, 70),
            (30, 90, 60),
            (240, 240, 240),
        ]);
        let seen = classify(&gold);
        assert!((seen.gold - 0.5).abs() < 1e-9 && seen.blue == 0.0);
        assert!(seen.gold_up() && seen.up());

        let navy = image(&[(49, 68, 124), (17, 28, 46), (201, 173, 151)]);
        let seen = classify(&navy);
        assert!((seen.blue - 1.0 / 3.0).abs() < 1e-9 && seen.gold == 0.0);
        assert!(!seen.gold_up() && seen.up());

        let felt = image(&[(17, 28, 46); 8]);
        assert!(!classify(&felt).up());
        assert_eq!(classify(&image(&[])), OkLook::default());
    }

    #[test]
    fn box_maps_into_device_pixels() {
        // The embedded view of the shots: a 1058×895 viewport, the canvas
        // full width and centred. The navy button there spans x 917–1013,
        // y 685–713; the gold one x 892–1030, y 675–715.
        let rect = CanvasRect {
            x: 0.0,
            y: 150.0,
            width: 1058.0,
            height: 595.125,
        };
        let (x0, y0, x1, y1) = ok_box_px(&rect, (1058.0, 895.0), (1058, 895));
        assert!(x0 >= 905 && x1 <= 1016, "x {x0}..{x1}");
        assert!(y0 >= 680 && y1 <= 716, "y {y0}..{y1}");
        // At 150 % scaling the capture is 1.5× the CSS viewport.
        let (a0, b0, a1, b1) = ok_box_px(&rect, (1058.0, 895.0), (1587, 1342));
        assert_eq!(
            (a0, a1),
            (
                (x0 as f64 * 1.5).round() as u32,
                (x1 as f64 * 1.5).round() as u32
            )
        );
        assert!(b0 > y0 && b1 > y1);
    }

    #[test]
    fn crop_takes_the_sub_rectangle() {
        // 3×2 image, pixel value = its index.
        let px = Pixels {
            width: 3,
            height: 2,
            rgb: (0u8..6).flat_map(|i| [i, i, i]).collect(),
        };
        let c = px.crop(1, 0, 3, 2);
        assert_eq!((c.width, c.height), (2, 2));
        assert_eq!(c.rgb, vec![1, 1, 1, 2, 2, 2, 4, 4, 4, 5, 5, 5]);
        let clamped = px.crop(2, 1, 9, 9);
        assert_eq!((clamped.width, clamped.height), (1, 1));
        assert_eq!(clamped.rgb, vec![5, 5, 5]);
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
        assert!((classify(&px).gold - 0.5).abs() < 1e-9);
    }
}
