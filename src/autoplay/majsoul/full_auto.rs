//! Mahjong Soul full-auto: queue a ranked match, let autoplay play it,
//! close the results, queue the next one.
//!
//! The lobby is driven the way the table is — synthesised clicks at
//! 16:9-normalised canvas coordinates ([`FullAutoCoords`]) — and checked
//! the way the table is: by what the client sends next. A ranked-mode press
//! is only taken as done once `startUnifiedMatch` has gone up the wire
//! (`autoplay::lobby`). A sequence that produced no such frame did not land
//! (a popup in the way, a result screen still up, a menu that was slower
//! than the click) and is retried; repeated misses reload the page, and a
//! page that will not come back stops the run with a reason.
//!
//! What it never does: act inside a game. From `authGame` to the game's
//! end, the hand belongs to the autoplay manager; full-auto only watches
//! for a stall (no game traffic for `game_stall_secs`) and reloads the page,
//! which reconnects into the hand through the bridge's `GameRestore` path.
//!
//! Running is runtime state ([`FullAutoControl`]), started and stopped from
//! the dashboard. Settings are read from `autoplay.full_auto` at the start
//! of every queue, so a change takes effect at the next game.

use crate::autoplay::cdp_input::{dispatch_click, dispatch_wheel, evaluate_canvas_rect};
use crate::autoplay::context::{AutoplayContext, CanvasRect};
use crate::autoplay::lobby::{self, LobbyWatch, QueueEvent};
use crate::config::{AppConfig, CaptureMode, FullAutoConfig, FullAutoMode, FullAutoRoom, Platform};
use crate::event_bus::NotifyBus;
use crate::schema::Notification;
use chromiumoxide::page::Page;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{watch, RwLock};
use tracing::{info, warn};

/// How often waits re-read the lobby slot.
const POLL: Duration = Duration::from_millis(250);
/// After a room press, how long its mode list may take to ask for queue
/// counts (`fetchCurrentMatchInfo`). Absent is tolerated — the mode press
/// is still verified — but a *wrong* room is caught here.
const ROOM_VIEW_WAIT: Duration = Duration::from_secs(4);
/// A room's mode list counts as open this long after its last poll (it
/// polls every ~7 s).
const ROOM_VIEW_FRESH: Duration = Duration::from_secs(9);
/// Room presses that opened another room than the one asked for, before the
/// run stops and asks for calibration.
const MAX_WRONG_ROOM: u32 = 2;
/// After a ranked-mode press, how long `startUnifiedMatch` may take to go up.
const QUEUE_REQUEST_WAIT: Duration = Duration::from_secs(6);
/// After the request, how long the server's answer may take.
const QUEUE_RESPONSE_WAIT: Duration = Duration::from_secs(6);
/// After a match is found, how long until the game socket authenticates.
const MATCH_TO_GAME_WAIT: Duration = Duration::from_secs(60);
/// After a reload, how long the lobby (or a reconnect into a game) may take.
const RELOAD_RECOVERY_WAIT: Duration = Duration::from_secs(90);
/// How long to wait for the game tab to exist at all.
const PAGE_WAIT: Duration = Duration::from_secs(60);
/// Queue sequences that may miss before the page is reloaded.
const MISSES_BEFORE_RELOAD: u32 = 2;
/// Reloads in a row without getting a queue in before the run stops.
const MAX_RELOADS: u32 = 3;
/// Click scatter around a target, in 16:9 units (~±7 px / ±5 px at 1080p).
const JITTER_X: f64 = 0.06;
const JITTER_Y: f64 = 0.04;
/// Room-list scroll: notches and their size.
const SCROLL_TICKS: u32 = 5;
const SCROLL_DELTA: f64 = 240.0;

// ---------- control + status (shared with IPC) ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FullAutoPhase {
    Idle,
    /// Waiting for the game tab / a login.
    WaitingLobby,
    /// Clicking through the lobby menus.
    Queueing,
    /// In the match queue.
    Queued,
    /// A match was found; the game socket is coming.
    Matched,
    InGame,
    /// Closing the result screens.
    Results,
    /// Resting in the lobby before the next queue.
    Resting,
    Reloading,
}

/// Why a run ended. `code` is stable (the frontend translates it);
/// `message` is the English detail for logs and toasts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopReason {
    pub code: String,
    pub message: String,
}

impl StopReason {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
    fn is_success(&self) -> bool {
        matches!(self.code.as_str(), "batch_done" | "user")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FullAutoStatus {
    pub running: bool,
    pub phase: FullAutoPhase,
    pub games_played: u32,
    pub max_games: u32,
    /// The room actually being queued in (`auto` resolved).
    pub room: Option<FullAutoRoom>,
    pub mode: FullAutoMode,
    /// What it is doing right now, in English (logs, tooltip).
    pub detail: Option<String>,
    /// Set when the last run ended.
    pub stop_reason: Option<StopReason>,
}

impl Default for FullAutoStatus {
    fn default() -> Self {
        Self {
            running: false,
            phase: FullAutoPhase::Idle,
            games_played: 0,
            max_games: 0,
            room: None,
            mode: FullAutoMode::default(),
            detail: None,
            stop_reason: None,
        }
    }
}

/// Start/stop switch and published status. Lives in [`AutoplayContext`],
/// so IPC commands and the runner share it without extra plumbing.
pub struct FullAutoControl {
    run: watch::Sender<bool>,
    status: watch::Sender<FullAutoStatus>,
}

impl Default for FullAutoControl {
    fn default() -> Self {
        Self {
            run: watch::Sender::new(false),
            status: watch::Sender::new(FullAutoStatus::default()),
        }
    }
}

impl FullAutoControl {
    /// Ask the runner to start. `false` if it was already running.
    pub fn start(&self) -> bool {
        !self.run.send_replace(true)
    }

    pub fn stop(&self) {
        self.run.send_replace(false);
    }

    pub fn is_running(&self) -> bool {
        *self.run.borrow()
    }

    pub fn status(&self) -> FullAutoStatus {
        self.status.borrow().clone()
    }

    pub fn subscribe_status(&self) -> watch::Receiver<FullAutoStatus> {
        self.status.subscribe()
    }

    fn publish(&self, f: impl FnOnce(&mut FullAutoStatus)) {
        self.status.send_modify(f);
    }
}

// ---------- runner ----------

/// Long-lived task: idles until started, runs one session, publishes why it
/// ended, idles again. Spawned once at startup.
pub async fn run_full_auto(
    cfg: Arc<RwLock<AppConfig>>,
    ctx: Arc<AutoplayContext>,
    notify: NotifyBus,
) {
    let control = ctx.full_auto.clone();
    let mut run_rx = control.run.subscribe();
    info!("full-auto: ready");
    loop {
        // Wait for a start.
        while !*run_rx.borrow_and_update() {
            if run_rx.changed().await.is_err() {
                return;
            }
        }
        let mut session = Session::new(cfg.clone(), ctx.clone(), control.clone());
        control.publish(|s| {
            *s = FullAutoStatus {
                running: true,
                phase: FullAutoPhase::WaitingLobby,
                ..FullAutoStatus::default()
            }
        });
        let _ = notify.send(Notification::info("Full auto started").id("full-auto"));
        info!("full-auto: started");

        let reason = tokio::select! {
            r = session.drive() => r,
            _ = wait_until_stopped(&mut run_rx) => {
                StopReason::new("user", "Stopped from the dashboard.")
            }
        };
        control.stop();
        info!("full-auto: stopped ({}): {}", reason.code, reason.message);
        let toast = if reason.is_success() {
            Notification::info("Full auto stopped")
        } else {
            Notification::warn("Full auto stopped")
        };
        let _ = notify.send(toast.body(reason.message.clone()).id("full-auto"));
        control.publish(|s| {
            s.running = false;
            s.phase = FullAutoPhase::Idle;
            s.detail = None;
            s.stop_reason = Some(reason);
        });
    }
}

async fn wait_until_stopped(rx: &mut watch::Receiver<bool>) {
    loop {
        if !*rx.borrow_and_update() {
            return;
        }
        if rx.changed().await.is_err() {
            // Sender gone: the process is exiting.
            std::future::pending::<()>().await;
        }
    }
}

type Step<T> = Result<T, StopReason>;

/// What one queue attempt achieved.
enum QueueOutcome {
    /// In the queue (or straight into a match).
    Queued,
    /// The presses produced no queue request.
    Missed,
}

struct Session {
    cfg: Arc<RwLock<AppConfig>>,
    ctx: Arc<AutoplayContext>,
    control: Arc<FullAutoControl>,
    games_played: u32,
    /// Reloads since the last successful queue.
    reloads: u32,
    /// Reloads during the current game (stall recovery).
    game_reloads: u32,
    /// Room presses in a row that opened the wrong room.
    wrong_room: u32,
}

impl Session {
    fn new(
        cfg: Arc<RwLock<AppConfig>>,
        ctx: Arc<AutoplayContext>,
        control: Arc<FullAutoControl>,
    ) -> Self {
        Self {
            cfg,
            ctx,
            control,
            games_played: 0,
            reloads: 0,
            game_reloads: 0,
            wrong_room: 0,
        }
    }

    fn watch(&self) -> LobbyWatch {
        self.ctx.lobby.read().map(|g| g.clone()).unwrap_or_default()
    }

    fn phase(&self, phase: FullAutoPhase, detail: impl Into<String>) {
        let detail = detail.into();
        info!("full-auto: {phase:?} — {detail}");
        self.control.publish(|s| {
            s.phase = phase;
            s.detail = Some(detail);
        });
    }

    /// The settings this step runs with, after the preconditions that make
    /// full-auto meaningful at all.
    async fn settings(&self) -> Step<(FullAutoConfig, crate::config::MajsoulAutoplayConfig)> {
        let cfg = self.cfg.read().await;
        if cfg.platform.kind != Platform::Majsoul {
            return Err(StopReason::new(
                "wrong_platform",
                "Full auto is for Mahjong Soul only.",
            ));
        }
        if cfg.capture.mode != CaptureMode::Chromium {
            return Err(StopReason::new(
                "needs_chromium",
                "Full auto needs the Chromium capture mode (it clicks inside the game window).",
            ));
        }
        if !cfg.autoplay.enabled {
            return Err(StopReason::new(
                "autoplay_off",
                "Autoplay is off, so nothing would play the matches. Turn it on in Settings.",
            ));
        }
        if !cfg.bot.enabled {
            return Err(StopReason::new(
                "bot_off",
                "The bot is off. Turn it on in Settings.",
            ));
        }
        Ok((cfg.autoplay.full_auto.clone(), cfg.autoplay.majsoul.clone()))
    }

    async fn drive(&mut self) -> StopReason {
        match self.drive_inner().await {
            Ok(()) => StopReason::new(
                "batch_done",
                format!("Finished {} game(s).", self.games_played),
            ),
            Err(r) => r,
        }
    }

    async fn drive_inner(&mut self) -> Step<()> {
        let (fa, _) = self.settings().await?;
        self.control.publish(|s| {
            s.max_games = fa.max_games;
            s.mode = fa.mode;
        });
        loop {
            if self.watch().in_game {
                self.play().await?;
                if self.batch_done().await {
                    return Ok(());
                }
                self.close_results().await?;
                continue;
            }
            self.ensure_lobby().await?;
            self.queue().await?;
            self.wait_for_game().await?;
        }
    }

    async fn batch_done(&self) -> bool {
        let max = self.cfg.read().await.autoplay.full_auto.max_games;
        max > 0 && self.games_played >= max
    }

    // ----- lobby -----

    /// Wait for the game tab and a login. A tab that has been open since
    /// before capture started may never show a login; one reload fixes that.
    async fn ensure_lobby(&mut self) -> Step<()> {
        self.phase(FullAutoPhase::WaitingLobby, "waiting for the game page");
        if self.wait_page(PAGE_WAIT).await.is_none() {
            return Err(StopReason::new(
                "no_page",
                "The game page is not open. Start capture and open Mahjong Soul.",
            ));
        }
        if self.watch().login_seq > 0 {
            return Ok(());
        }
        self.phase(FullAutoPhase::WaitingLobby, "waiting for a login");
        if self
            .wait_for(Duration::from_secs(20), |w| w.login_seq > 0)
            .await
            .is_some()
        {
            return Ok(());
        }
        // Logged in before capture saw it: a reload logs in again, on the wire.
        self.reload("no login seen yet").await?;
        if self.watch().login_seq > 0 || self.watch().in_game {
            return Ok(());
        }
        Err(StopReason::new(
            "no_login",
            "No login was seen. Log in to Mahjong Soul in the game window, then start again.",
        ))
    }

    /// The room to queue in, `auto` resolved against the account's rank.
    fn resolve_room(&self, fa: &FullAutoConfig) -> Step<usize> {
        let level = self.watch().level_for(fa.mode.is_sanma());
        match fa.room.index() {
            Some(room) => {
                if let Some(level) = level {
                    if !lobby::room_admits(room, level) {
                        return Err(StopReason::new(
                            "room_not_admitted",
                            format!(
                                "Rank {level} cannot enter the {:?} room. Pick a lower room or Auto.",
                                fa.room
                            ),
                        ));
                    }
                }
                Ok(room)
            }
            None => match level {
                Some(level) => Ok(lobby::best_room_for(level)),
                None => Err(StopReason::new(
                    "rank_unknown",
                    "Room is Auto but the account's rank is not known yet. Pick a room explicitly.",
                )),
            },
        }
    }

    /// Get into a ranked queue, retrying and reloading as needed.
    async fn queue(&mut self) -> Step<()> {
        let mut misses = 0u32;
        loop {
            let (fa, ms) = self.settings().await?;
            let room = self.resolve_room(&fa)?;
            self.control
                .publish(|s| s.room = Some(FullAutoRoom::from_index(room)));
            match self.queue_once(&fa, &ms, room).await? {
                QueueOutcome::Queued => {
                    self.reloads = 0;
                    return Ok(());
                }
                QueueOutcome::Missed => {
                    misses += 1;
                    warn!("full-auto: queue attempt {misses} produced no queue request");
                    if misses >= MISSES_BEFORE_RELOAD {
                        misses = 0;
                        self.reload("the lobby presses are not landing").await?;
                        if self.watch().in_game {
                            // The reload reconnected into a match after all.
                            return Ok(());
                        }
                    } else {
                        // Most often a menu is left half-open: back out to
                        // the front page and start over.
                        self.back_out(&fa, &ms, 2).await?;
                    }
                }
            }
        }
    }

    /// One pass through the menus: Ranked → room (scrolling the list for
    /// the upper rooms) → mode, then wait for the queue request.
    async fn queue_once(
        &mut self,
        fa: &FullAutoConfig,
        ms: &crate::config::MajsoulAutoplayConfig,
        room: usize,
    ) -> Step<QueueOutcome> {
        let c = &fa.coords;
        let expected = lobby::expected_mode_id(room, fa.mode);
        self.phase(
            FullAutoPhase::Queueing,
            format!(
                "queueing {:?} {:?}",
                FullAutoRoom::from_index(room),
                fa.mode
            ),
        );
        let ticket = self.watch().queue_seq;
        let step = || draw(fa.step_delay_ms);

        // A room's mode list re-polls its queue counts every few seconds
        // while it is open: if one is open, back out of it first, or the
        // presses below land on its entries instead.
        if self.room_view_open() {
            self.back_out(fa, ms, 2).await?;
        }

        self.press(ms, c.ranked).await?;
        self.sleep(step()).await;
        if room >= c.room_scroll_from {
            self.scroll(c.room_scroll_anchor).await?;
            self.sleep(step()).await;
        }
        let view_ticket = self.watch().room_view_seq;
        self.press(ms, c.rooms[room.min(4)]).await?;
        // The room's mode list asks for its queue counts as it opens; the
        // ids it asks about say which room actually opened.
        if let Some(w) = self
            .wait_for(ROOM_VIEW_WAIT, |w| w.room_view_seq > view_ticket)
            .await
        {
            match lobby::room_of_mode_list(&w.room_view_modes) {
                Some(opened) if opened != room => {
                    self.wrong_room += 1;
                    warn!(
                        "full-auto: the room press opened room {opened}, not {room} \
                         ({} in a row)",
                        self.wrong_room
                    );
                    if self.wrong_room >= MAX_WRONG_ROOM {
                        return Err(StopReason::new(
                            "calibrate",
                            format!(
                                "The {:?} button opened the {:?} room. Calibrate the room positions.",
                                FullAutoRoom::from_index(room),
                                FullAutoRoom::from_index(opened)
                            ),
                        ));
                    }
                    return Ok(QueueOutcome::Missed);
                }
                _ => self.wrong_room = 0,
            }
        }
        self.sleep(step()).await;
        self.press(ms, c.modes[fa.mode.index()]).await?;

        // The press is only done once the client asked for the queue.
        let Some(w) = self
            .wait_for(QUEUE_REQUEST_WAIT, |w| {
                w.queue_seq > ticket && w.queue != QueueEvent::None
            })
            .await
        else {
            return Ok(QueueOutcome::Missed);
        };
        if let QueueEvent::Requested { mode_id } = w.queue {
            if let Some(id) = mode_id {
                if id != expected {
                    warn!(
                        "full-auto: queued mode {id}, expected {expected} — \
                         the room/mode coordinates may need calibrating"
                    );
                }
            }
            // Wait for the server's answer to the request.
            let seq = w.queue_seq;
            // No answer seen: the request went up; treat it as queued and
            // let the queue timeout catch a dead one.
            if let Some(w) = self
                .wait_for(QUEUE_RESPONSE_WAIT, |w| w.queue_seq > seq)
                .await
            {
                self.judge_queue(&w.queue)?;
            }
        } else {
            self.judge_queue(&w.queue)?;
        }
        self.phase(FullAutoPhase::Queued, "in the match queue");
        Ok(QueueOutcome::Queued)
    }

    fn judge_queue(&self, ev: &QueueEvent) -> Step<()> {
        match ev {
            QueueEvent::Rejected { code } => Err(StopReason::new(
                "queue_rejected",
                format!("The server refused the queue request (error {code})."),
            )),
            _ => Ok(()),
        }
    }

    /// In the queue until the game socket authenticates.
    async fn wait_for_game(&mut self) -> Step<()> {
        let (fa, _) = self.settings().await?;
        let timeout = Duration::from_secs(u64::from(fa.queue_timeout_secs.max(30)));
        let start = Instant::now();
        loop {
            let w = self.watch();
            if w.in_game {
                return Ok(());
            }
            match w.queue {
                QueueEvent::Matched { .. } => {
                    self.phase(FullAutoPhase::Matched, "match found");
                    if self
                        .wait_for(MATCH_TO_GAME_WAIT, |w| w.in_game)
                        .await
                        .is_some()
                    {
                        return Ok(());
                    }
                    // The game never connected; a reload rejoins it if it
                    // exists.
                    self.reload("the match never connected").await?;
                    return Ok(());
                }
                QueueEvent::Dropped | QueueEvent::Cancelled => {
                    info!("full-auto: the queue ended without a match; queueing again");
                    return Ok(());
                }
                _ => {}
            }
            if start.elapsed() > timeout {
                self.reload("no match within the queue timeout").await?;
                return Ok(());
            }
            if !self.control.is_running() {
                return Ok(());
            }
            tokio::time::sleep(POLL).await;
        }
    }

    // ----- in game -----

    /// Watch a game to its end. Autoplay plays it; this only recovers a
    /// stalled page.
    async fn play(&mut self) -> Step<()> {
        self.phase(
            FullAutoPhase::InGame,
            format!("playing game {}", self.games_played + 1),
        );
        self.game_reloads = 0;
        let end_ticket = self.watch().game_end_seq;
        loop {
            let w = self.watch();
            if w.game_end_seq > end_ticket {
                self.games_played += 1;
                let n = self.games_played;
                self.control.publish(|s| s.games_played = n);
                info!(
                    "full-auto: game {n} {}",
                    if w.last_end_terminated {
                        "terminated"
                    } else {
                        "finished"
                    }
                );
                return Ok(());
            }
            let (fa, _) = self.settings().await?;
            let stall = Duration::from_secs(u64::from(fa.game_stall_secs.max(60)));
            let stalled = w.last_game_frame_at.is_none_or(|t| t.elapsed() > stall);
            if stalled {
                self.game_reloads += 1;
                if self.game_reloads > MAX_RELOADS {
                    return Err(StopReason::new(
                        "page_dead",
                        "The game stopped responding and reloading did not bring it back.",
                    ));
                }
                self.reload("no game traffic for a long time").await?;
                if !self.watch().in_game && self.watch().game_end_seq == end_ticket {
                    // Back in the lobby with no end seen: the game is gone
                    // (ended while we were away). Count nothing; queue again.
                    return Ok(());
                }
                // Reconnected: the stall clock restarts from the reconnect.
                continue;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    /// Press through the result screens, then rest before the next queue.
    ///
    /// Stops pressing as soon as the client reloads its own profile — its
    /// cue for being back on the lobby — because the OK spot there sits
    /// next to the gacha entry. Without the cue it stops after
    /// `result_presses`; the queue attempt that follows verifies the rest.
    async fn close_results(&mut self) -> Step<()> {
        let (fa, ms) = self.settings().await?;
        self.phase(FullAutoPhase::Results, "closing the result screens");
        let lobby_ticket = self.watch().self_info_seq;
        let back = |w: &LobbyWatch| w.self_info_seq > lobby_ticket || w.in_game;
        self.sleep(draw(fa.result_delay_ms)).await;
        for _ in 0..fa.result_presses {
            if back(&self.watch()) {
                break;
            }
            self.press(&ms, fa.coords.result_ok).await?;
            // Wait out the gap, but leave it early on the cue.
            let gap = Duration::from_millis(u64::from(draw(fa.result_gap_ms)));
            if self.wait_for(gap, back).await.is_some() {
                break;
            }
        }
        if self.watch().in_game {
            return Ok(());
        }
        self.phase(FullAutoPhase::Resting, "resting before the next game");
        self.sleep(draw(fa.rest_between_games_ms)).await;
        Ok(())
    }

    /// Whether a room's mode list is on screen: it polls
    /// `fetchCurrentMatchInfo` every ~7 s while it is.
    fn room_view_open(&self) -> bool {
        self.watch()
            .room_view_at
            .is_some_and(|t| t.elapsed() < ROOM_VIEW_FRESH)
    }

    /// Press Back `times` times (mode list → room list → front page).
    async fn back_out(
        &self,
        fa: &FullAutoConfig,
        ms: &crate::config::MajsoulAutoplayConfig,
        times: u32,
    ) -> Step<()> {
        for _ in 0..times {
            self.press(ms, fa.coords.back).await?;
            self.sleep(draw(fa.step_delay_ms)).await;
        }
        Ok(())
    }

    // ----- primitives -----

    async fn page(&self) -> Option<Page> {
        self.ctx.page.read().await.as_ref().cloned()
    }

    async fn wait_page(&self, timeout: Duration) -> Option<Page> {
        let start = Instant::now();
        loop {
            if let Some(p) = self.page().await {
                return Some(p);
            }
            if start.elapsed() > timeout {
                return None;
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Poll the lobby slot until `cond` holds or `timeout` passes.
    async fn wait_for(
        &self,
        timeout: Duration,
        cond: impl Fn(&LobbyWatch) -> bool,
    ) -> Option<LobbyWatch> {
        let start = Instant::now();
        loop {
            let w = self.watch();
            if cond(&w) {
                return Some(w);
            }
            if start.elapsed() > timeout {
                return None;
            }
            tokio::time::sleep(POLL).await;
        }
    }

    async fn sleep(&self, ms: u32) {
        tokio::time::sleep(Duration::from_millis(u64::from(ms))).await;
    }

    async fn canvas(&self, page: &Page) -> Step<CanvasRect> {
        match evaluate_canvas_rect(page).await {
            Ok(r) if r.width > 0.0 && r.height > 0.0 => Ok(r),
            Ok(_) => Err(StopReason::new("no_canvas", "The game canvas has no size.")),
            Err(e) => {
                warn!("full-auto: canvas rect failed: {e:#}");
                Err(StopReason::new(
                    "no_canvas",
                    "Could not find the game canvas.",
                ))
            }
        }
    }

    /// One press at a 16:9 point, scattered a little, with the same hover
    /// and hold shaping the in-game clicks use.
    async fn press(&self, ms: &crate::config::MajsoulAutoplayConfig, at: [f64; 2]) -> Step<()> {
        let Some(page) = self.wait_page(PAGE_WAIT).await else {
            return Err(StopReason::new("no_page", "The game page went away."));
        };
        let rect = self.canvas(&page).await?;
        let drawn = ms.draw(&mut rand::rng());
        let (x, y) = {
            let mut rng = rand::rng();
            let jx = rng.random_range(-JITTER_X..=JITTER_X);
            let jy = rng.random_range(-JITTER_Y..=JITTER_Y);
            rect.pixel(at[0] + jx, at[1] + jy)
        };
        if !rect.contains(x, y) {
            warn!("full-auto: target {at:?} is off the canvas; skipped");
            return Ok(());
        }
        if let Err(e) = dispatch_click(
            &page,
            x,
            y,
            drawn.hover_delay_ms.ms(),
            drawn.click_hold_ms.ms(),
        )
        .await
        {
            warn!("full-auto: click failed: {e:#}");
        }
        Ok(())
    }

    async fn scroll(&self, at: [f64; 2]) -> Step<()> {
        let Some(page) = self.wait_page(PAGE_WAIT).await else {
            return Err(StopReason::new("no_page", "The game page went away."));
        };
        let rect = self.canvas(&page).await?;
        let (x, y) = rect.pixel(at[0], at[1]);
        let gap = rand_ms(40, 110);
        if let Err(e) = dispatch_wheel(&page, x, y, SCROLL_DELTA, SCROLL_TICKS, gap).await {
            warn!("full-auto: scroll failed: {e:#}");
        }
        Ok(())
    }

    /// Reload the game tab and wait for it to come back to a login or a
    /// game. Counts against [`MAX_RELOADS`].
    async fn reload(&mut self, why: &str) -> Step<()> {
        self.reloads += 1;
        if self.reloads > MAX_RELOADS {
            return Err(StopReason::new(
                "page_dead",
                format!("Reloaded {MAX_RELOADS} times without recovering ({why})."),
            ));
        }
        self.phase(
            FullAutoPhase::Reloading,
            format!("reloading the page: {why}"),
        );
        let Some(page) = self.wait_page(PAGE_WAIT).await else {
            return Err(StopReason::new("no_page", "The game page went away."));
        };
        let before = self.watch();
        if let Err(e) = page.reload().await {
            warn!("full-auto: reload failed: {e:#}");
        }
        *self.ctx.canvas_rect.write().await = None;
        let came_back = self
            .wait_for(RELOAD_RECOVERY_WAIT, |w| {
                w.login_seq > before.login_seq || w.game_seq > before.game_seq
            })
            .await;
        if came_back.is_none() {
            warn!("full-auto: no login or game after the reload");
        }
        // The lobby keeps animating in after the login response.
        self.sleep(rand_ms(4000, 7000)).await;
        Ok(())
    }
}

/// One draw from a range. The RNG is dropped before the caller awaits:
/// `ThreadRng` is not `Send`, and the runner is a spawned task.
fn draw(r: crate::config::MsRange) -> u32 {
    r.draw(&mut rand::rng()).ms()
}

fn rand_ms(lo: u32, hi: u32) -> u32 {
    rand::rng().random_range(lo..=hi)
}

// ---------- calibration ----------

/// Where a calibrated point goes in [`crate::config::FullAutoCoords`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "index", rename_all = "snake_case")]
pub enum CalibrationTarget {
    Ranked,
    Room(usize),
    Mode(usize),
    ResultOk,
    Back,
    ScrollAnchor,
    /// The hand-result screens' OK (`autoplay.majsoul.round_ok`) — not a
    /// full-auto coordinate, but calibrated the same way.
    RoundOk,
}

impl CalibrationTarget {
    /// Store `at` into `coords`. `false` for an out-of-range index.
    pub fn apply(self, coords: &mut crate::config::FullAutoCoords, at: [f64; 2]) -> bool {
        match self {
            Self::Ranked => coords.ranked = at,
            Self::Room(i) if i < 5 => coords.rooms[i] = at,
            Self::Mode(i) if i < 4 => coords.modes[i] = at,
            Self::ResultOk => coords.result_ok = at,
            Self::Back => coords.back = at,
            Self::ScrollAnchor => coords.room_scroll_anchor = at,
            // Lives in the majsoul section; the caller stores it.
            Self::RoundOk => return false,
            _ => return false,
        }
        true
    }
}

/// Arm a one-shot listener in the game page and return the next real press
/// on the canvas, in 16:9 units. The listener runs in the capture phase, so
/// the game still gets the click as usual.
pub async fn record_next_click(
    ctx: &AutoplayContext,
    timeout: Duration,
) -> anyhow::Result<[f64; 2]> {
    let page = ctx
        .page
        .read()
        .await
        .as_ref()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("the game page is not open"))?;
    let arm = "(()=>{window.__akagiCalib=null;\
               if(!window.__akagiCalibArmed){window.__akagiCalibArmed=true;\
               window.addEventListener('pointerdown',e=>{if(e.isTrusted){\
               window.__akagiCalib={x:e.clientX,y:e.clientY};}},true);}\
               return true;})()";
    page.evaluate(arm).await?;
    let start = Instant::now();
    loop {
        let v: Option<serde_json::Value> = page
            .evaluate("(()=>window.__akagiCalib||null)()")
            .await?
            .into_value()
            .ok();
        if let Some(v) = v.filter(|v| !v.is_null()) {
            let x = v.get("x").and_then(serde_json::Value::as_f64);
            let y = v.get("y").and_then(serde_json::Value::as_f64);
            if let (Some(x), Some(y)) = (x, y) {
                let rect = evaluate_canvas_rect(&page).await?;
                return to_normalised(&rect, x, y)
                    .ok_or_else(|| anyhow::anyhow!("the click was outside the game canvas"));
            }
        }
        if start.elapsed() > timeout {
            anyhow::bail!("no click within {}s", timeout.as_secs());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// CSS pixels → 16:9 units of `rect`, rounded to 3 decimals. `None` off
/// the canvas.
pub fn to_normalised(rect: &CanvasRect, x: f64, y: f64) -> Option<[f64; 2]> {
    if !rect.contains(x, y) || rect.width <= 0.0 || rect.height <= 0.0 {
        return None;
    }
    let nx = (x - rect.x) / rect.width * 16.0;
    let ny = (y - rect.y) / rect.height * 9.0;
    let r = |v: f64| (v * 1000.0).round() / 1000.0;
    Some([r(nx), r(ny)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_start_stop() {
        let c = FullAutoControl::default();
        assert!(!c.is_running());
        assert!(c.start());
        assert!(!c.start(), "second start is a no-op");
        assert!(c.is_running());
        c.stop();
        assert!(!c.is_running());
    }

    #[test]
    fn normalising_round_trips_pixel() {
        let rect = CanvasRect {
            x: 100.0,
            y: 50.0,
            width: 1280.0,
            height: 720.0,
        };
        let (px, py) = rect.pixel(11.5, 2.75);
        assert_eq!(to_normalised(&rect, px, py), Some([11.5, 2.75]));
        assert_eq!(to_normalised(&rect, 0.0, 0.0), None);
    }

    #[test]
    fn calibration_targets_land_in_their_slot() {
        let mut c = crate::config::FullAutoCoords::default();
        assert!(CalibrationTarget::Room(3).apply(&mut c, [1.0, 2.0]));
        assert_eq!(c.rooms[3], [1.0, 2.0]);
        assert!(CalibrationTarget::Mode(1).apply(&mut c, [3.0, 4.0]));
        assert_eq!(c.modes[1], [3.0, 4.0]);
        assert!(!CalibrationTarget::Room(5).apply(&mut c, [0.0, 0.0]));
    }

    #[test]
    fn calibration_target_wire_shape() {
        let t: CalibrationTarget = serde_json::from_str(r#"{"kind":"room","index":2}"#).unwrap();
        assert_eq!(t, CalibrationTarget::Room(2));
        let t: CalibrationTarget = serde_json::from_str(r#"{"kind":"ranked"}"#).unwrap();
        assert_eq!(t, CalibrationTarget::Ranked);
    }

    #[test]
    fn status_serialises_for_the_frontend() {
        let s = FullAutoStatus {
            stop_reason: Some(StopReason::new("batch_done", "Finished 3 game(s).")),
            ..FullAutoStatus::default()
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["phase"], "idle");
        assert_eq!(v["mode"], "south4");
        assert_eq!(v["stop_reason"]["code"], "batch_done");
    }
}
