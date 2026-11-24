//! Mahjong Soul hand-result screens: press OK so the next hand is dealt.
//!
//! After every hand that ends before the game does (a win, an exhaustive
//! draw, an abortive draw), the client shows one or two result pages (the
//! win and its yaku, then the score change) and deals the next hand only
//! after the player closes them — or after its own ~15 s countdown. Closing
//! the last page is visible on the wire: the client sends
//! `.lq.FastTest.confirmNewRound`.
//!
//! That request is the gate, and the reason this can press blind. Presses
//! happen only between the hand's end (`ActionHule` / `ActionNoTile` /
//! `ActionLiuJu`) and either the client's `confirmNewRound`, the next deal,
//! or the game's end — the window in which the result pages cover the
//! table and there is no hand to touch. The game's last hand is followed by
//! the game-end notice within milliseconds, so it is never pressed here:
//! its result screens belong to full-auto (or to the player).
//!
//! Each press is followed by a wait for that request; the first that comes
//! ends the round's presses. If none comes after `round_confirm_presses`,
//! the client's countdown finishes the job, and the log says the OK
//! position may need calibrating.

use crate::autoplay::cdp_input::{dispatch_click, evaluate_canvas_rect, move_to};
use crate::autoplay::context::AutoplayContext;
use crate::autoplay::lobby::LobbyWatch;
use crate::autoplay::majsoul::coords::CURSOR_REST;
use crate::config::{AppConfig, MajsoulAutoplayConfig, Platform};
use rand::Rng;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

const POLL: Duration = Duration::from_millis(250);
/// Click scatter around the OK button, 16:9 units.
const JITTER: (f64, f64) = (0.07, 0.04);

fn watch(ctx: &AutoplayContext) -> LobbyWatch {
    ctx.lobby.read().map(|g| g.clone()).unwrap_or_default()
}

/// Whether the result pages of the hand that ended at `start` can still be
/// up: still in that game, and nothing has moved past them.
fn results_up(start: &LobbyWatch, now: &LobbyWatch) -> bool {
    now.in_game
        && now.game_end_seq == start.game_end_seq
        && now.game_seq == start.game_seq
        && now.round_end_seq == start.round_end_seq
        && now.new_round_seq == start.new_round_seq
        && now.round_confirm_seq == start.round_confirm_seq
}

/// Sleep `ms` while `results_up` holds; `false` as soon as it stops.
async fn hold(ctx: &AutoplayContext, start: &LobbyWatch, ms: u32) -> bool {
    let until = Instant::now() + Duration::from_millis(u64::from(ms));
    loop {
        if !results_up(start, &watch(ctx)) {
            return false;
        }
        let now = Instant::now();
        if now >= until {
            return true;
        }
        tokio::time::sleep(POLL.min(until - now)).await;
    }
}

async fn settings(cfg: &Arc<RwLock<AppConfig>>) -> Option<MajsoulAutoplayConfig> {
    let cfg = cfg.read().await;
    let on = cfg.autoplay.enabled
        && cfg.platform.kind == Platform::Majsoul
        && cfg.autoplay.majsoul.auto_round_confirm;
    on.then(|| cfg.autoplay.majsoul.draw(&mut rand::rng()))
}

/// Long-lived task, spawned once at startup; idle unless autoplay is on for
/// Mahjong Soul with `auto_round_confirm`.
pub async fn run_round_advance(cfg: Arc<RwLock<AppConfig>>, ctx: Arc<AutoplayContext>) {
    let mut seen = watch(&ctx).round_end_seq;
    loop {
        tokio::time::sleep(POLL).await;
        let start = watch(&ctx);
        if start.round_end_seq == seen {
            continue;
        }
        seen = start.round_end_seq;
        let Some(ms) = settings(&cfg).await else {
            continue;
        };
        confirm_hand(&ctx, &ms, &start).await;
        seen = watch(&ctx).round_end_seq;
    }
}

async fn confirm_hand(ctx: &AutoplayContext, ms: &MajsoulAutoplayConfig, start: &LobbyWatch) {
    if !hold(ctx, start, ms.round_confirm_delay_ms.ms()).await {
        // The game ended with this hand, or the player was quicker.
        return;
    }
    for n in 1..=ms.round_confirm_presses {
        press(ctx, ms, ms.round_ok).await;
        let gap = ms.round_confirm_gap_ms.draw(&mut rand::rng()).ms();
        if !hold(ctx, start, gap).await {
            let w = watch(ctx);
            if w.round_confirm_seq > start.round_confirm_seq {
                info!("autoplay: hand results confirmed after {n} press(es)");
            }
            park(ctx).await;
            return;
        }
    }
    warn!(
        "autoplay: {} presses on the hand-result OK brought no confirmNewRound; \
         the client's countdown will advance — `autoplay.majsoul.round_ok` may need calibrating",
        ms.round_confirm_presses
    );
    park(ctx).await;
}

async fn press(ctx: &AutoplayContext, ms: &MajsoulAutoplayConfig, at: [f64; 2]) {
    let page = ctx.page.read().await.as_ref().cloned();
    let Some(page) = page else { return };
    let rect = match evaluate_canvas_rect(&page).await {
        Ok(r) if r.width > 0.0 && r.height > 0.0 => r,
        Ok(_) => return,
        Err(e) => {
            debug!("autoplay: hand-result OK: no canvas rect: {e:#}");
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
    if let Err(e) = dispatch_click(&page, x, y, ms.hover_delay_ms.ms(), ms.click_hold_ms.ms()).await
    {
        debug!("autoplay: hand-result OK press failed: {e:#}");
    }
}

/// Pointer back to the rest area (see `coords::CURSOR_REST`).
async fn park(ctx: &AutoplayContext) {
    let page = ctx.page.read().await.as_ref().cloned();
    let Some(page) = page else { return };
    let Ok(rect) = evaluate_canvas_rect(&page).await else {
        return;
    };
    let ((x0, x1), (y0, y1)) = CURSOR_REST;
    let (nx, ny) = {
        let mut rng = rand::rng();
        (rng.random_range(x0..=x1), rng.random_range(y0..=y1))
    };
    let (x, y) = rect.pixel(nx, ny);
    let _ = move_to(&page, x, y).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ended() -> LobbyWatch {
        LobbyWatch {
            in_game: true,
            game_seq: 1,
            round_end_seq: 3,
            new_round_seq: 2,
            round_confirm_seq: 2,
            ..LobbyWatch::default()
        }
    }

    #[test]
    fn results_are_up_until_something_moves_past_them() {
        let s = ended();
        assert!(results_up(&s, &s));
        let mut w = s.clone();
        w.round_confirm_seq += 1;
        assert!(!results_up(&s, &w), "the client confirmed");
        let mut w = s.clone();
        w.new_round_seq += 1;
        assert!(!results_up(&s, &w), "the next hand was dealt");
        let mut w = s.clone();
        w.in_game = false;
        w.game_end_seq += 1;
        assert!(!results_up(&s, &w), "the game ended with this hand");
        let mut w = s.clone();
        w.game_seq += 1;
        assert!(!results_up(&s, &w), "a reconnect into another game");
    }

    /// The game's last hand: the end notice is in the same burst as the
    /// win, so by the first press the gate is already shut.
    #[test]
    fn last_hand_is_never_pressed() {
        let s = ended();
        let mut after = s.clone();
        after.in_game = false;
        after.game_end_seq += 1;
        assert!(!results_up(&s, &after));
    }

    #[test]
    fn rest_area_avoids_the_hand_and_buttons() {
        let ((x0, x1), (y0, y1)) = CURSOR_REST;
        // Left of the leftmost action button's left edge (6.4 − ~0.95).
        assert!(x1 < 5.45);
        // Above raised hand tiles, below the candidate row.
        assert!(y1 < 7.5 && y0 > 6.7);
        // Clear of the left-edge auto-settings bar.
        assert!(x0 > 1.0);
    }
}
