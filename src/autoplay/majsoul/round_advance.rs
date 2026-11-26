//! Mahjong Soul hand-result screens: press OK so the next hand is dealt.
//!
//! After every hand that ends before the game does (a win, an exhaustive
//! draw, an abortive draw), the client shows one or two result pages (the
//! win and its yaku, then the score change) and deals the next hand only
//! after the player closes them — or after its own countdown. Closing the
//! last page is visible on the wire: the client sends
//! `.lq.FastTest.confirmNewRound`.
//!
//! Each page's OK (a navy "(3)確認" with the countdown) only takes a press
//! once it is on screen, 8–10 s after the win for the first page, so
//! presses are made when the button is seen
//! (`majsoul::sight`), a reaction time after it appears. If the screen
//! cannot be read, presses fall back to a timer and keep going until the
//! client confirms.
//!
//! The wire is the gate. Presses happen only between the hand's end
//! (`ActionHule` / `ActionNoTile` / `ActionLiuJu`) and either the client's
//! `confirmNewRound`, the next deal, or the game's end — the window in
//! which the result pages cover the table and there is no hand to touch.
//! The game's last hand is followed by the game-end notice within
//! milliseconds, so it is never pressed here: its result screens belong to
//! full-auto (or to the player).

use crate::autoplay::cdp_input::{evaluate_canvas_rect, move_to};
use crate::autoplay::context::AutoplayContext;
use crate::autoplay::lobby::LobbyWatch;
use crate::autoplay::majsoul::coords::CURSOR_REST;
use crate::autoplay::majsoul::sight::{self, PagesEnd};
use crate::config::{AppConfig, MajsoulAutoplayConfig, Platform};
use rand::Rng;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{info, warn};

const POLL: Duration = Duration::from_millis(250);
/// The longest a hand's result pages are watched. The client's own
/// countdown has closed them well before this.
const PAGES_LIMIT: Duration = Duration::from_secs(40);
/// No OK seen this long after a hand's end: save a screenshot (the first
/// page's button is normally up 8–10 s in).
const DIAGNOSE_AFTER: Duration = Duration::from_secs(14);

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
    let end = sight::press_through_pages(
        ctx,
        ms,
        ms.round_ok,
        PAGES_LIMIT,
        Some((DIAGNOSE_AFTER, "hand-ok-not-seen")),
        |_| !results_up(start, &watch(ctx)),
    )
    .await;
    match end {
        PagesEnd::Done { presses } => {
            let w = watch(ctx);
            if w.round_confirm_seq > start.round_confirm_seq && presses > 0 {
                info!("autoplay: hand results confirmed after {presses} press(es)");
            }
        }
        PagesEnd::TimedOut { presses } => {
            warn!(
                "autoplay: hand results still up after {}s and {presses} press(es)",
                PAGES_LIMIT.as_secs()
            );
        }
        PagesEnd::SightLost { presses } => {
            warn!(
                "autoplay: cannot read the screen for the hand-result OK ({presses} press(es) so far); \
                 pressing on a timer"
            );
            confirm_hand_blind(ctx, ms, start).await;
        }
    }
    park(ctx).await;
}

/// Timer presses, for when the screen cannot be read: keep pressing until
/// the client confirms (or its countdown does).
async fn confirm_hand_blind(ctx: &AutoplayContext, ms: &MajsoulAutoplayConfig, start: &LobbyWatch) {
    if !hold(ctx, start, ms.round_confirm_delay_ms.ms()).await {
        return;
    }
    let presses = ms.round_confirm_presses.max(6);
    for n in 1..=presses {
        sight::press(ctx, ms, ms.round_ok).await;
        let gap = ms.round_confirm_gap_ms.draw(&mut rand::rng()).ms();
        if !hold(ctx, start, gap).await {
            let w = watch(ctx);
            if w.round_confirm_seq > start.round_confirm_seq {
                info!("autoplay: hand results confirmed after {n} blind press(es)");
            }
            return;
        }
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
