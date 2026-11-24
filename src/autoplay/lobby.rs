//! Mahjong Soul lobby state, read off the wire for full-auto.
//!
//! The lobby is a canvas like the table: nothing on screen can be asked
//! whether a menu opened or a button took. What the client *sends* can — a
//! press on a ranked mode is followed by `.lq.Lobby.startUnifiedMatch`, a
//! login by its response, a found match by `.lq.NotifyMatchGameStart`. So
//! full-auto clicks, then waits for the frame that proves the click did
//! what it was meant to, the same way `autoplay::verify` proves a discard
//! landed.
//!
//! Written by every Majsoul bridge (lobby and game sockets are separate
//! flows with separate bridges; both are handed this slot through
//! `BridgeHooks::lobby_watch`), read by `autoplay::majsoul::full_auto`.
//! Each kind of fact carries a sequence number, so the reader takes a
//! ticket before acting and asks afterwards whether anything new arrived.

use serde_json::Value as JsonValue;
use std::sync::{Arc, RwLock};
use std::time::Instant;

pub const METHOD_START_UNIFIED_MATCH: &str = ".lq.Lobby.startUnifiedMatch";
pub const METHOD_CANCEL_UNIFIED_MATCH: &str = ".lq.Lobby.cancelUnifiedMatch";
pub const METHOD_NOTIFY_MATCH_GAME_START: &str = ".lq.NotifyMatchGameStart";
pub const METHOD_NOTIFY_MATCH_TIMEOUT: &str = ".lq.NotifyMatchTimeout";
pub const METHOD_NOTIFY_MATCH_FAILED: &str = ".lq.NotifyMatchFailed";
pub const METHOD_NOTIFY_ACCOUNT_LEVEL_CHANGE: &str = ".lq.NotifyAccountLevelChange";
/// Sent when a ranked room's mode list is on screen (and re-polled every
/// few seconds while it stays there) to show how many are queueing. Its
/// `mode_list` names the room: Bronze asks for `[2, 3, 17, 18]`.
pub const METHOD_FETCH_CURRENT_MATCH_INFO: &str = ".lq.Lobby.fetchCurrentMatchInfo";
/// The client reloads its own profile (`account_id: 0`) when it is back on
/// the lobby after a game's result screens — the cue to stop pressing the
/// result-screen OK, whose spot in the lobby is next to the gacha.
pub const METHOD_FETCH_ACCOUNT_INFO: &str = ".lq.Lobby.fetchAccountInfo";
/// Sent right after a game ends; carries the rank change of that game in
/// `level_change` (there is no separate `NotifyAccountLevelChange` then).
pub const METHOD_NOTIFY_GAME_FINISH_REWARD: &str = ".lq.NotifyGameFinishRewardV2";
const METHOD_AUTH_GAME: &str = ".lq.FastTest.authGame";
const METHOD_ACTION_PROTOTYPE: &str = ".lq.ActionPrototype";
const METHOD_NOTIFY_GAME_END_RESULT: &str = ".lq.NotifyGameEndResult";
const METHOD_NOTIFY_GAME_TERMINATE: &str = ".lq.NotifyGameTerminate";
/// Sent by the client when the player closes a hand's last result page
/// (or its countdown runs out); the next hand is dealt only after it.
pub const METHOD_CONFIRM_NEW_ROUND: &str = ".lq.FastTest.confirmNewRound";

/// Which way a frame went and what it was, as far as the lobby cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LobbyFrame {
    Request,
    Response,
    Notify,
}

/// The last thing that happened to the match queue.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum QueueEvent {
    #[default]
    None,
    /// The client asked to join a queue (`startUnifiedMatch` went up).
    /// `mode_id` is parsed from its `match_sid` (`"1:12"` → 12).
    Requested { mode_id: Option<u32> },
    /// The server accepted the queue request.
    Accepted,
    /// The server refused it; `code` is its `Error.code`.
    Rejected { code: u32 },
    /// The client left the queue.
    Cancelled,
    /// The queue timed out or failed on the server's side.
    Dropped,
    /// A match was found; the game socket comes next.
    Matched { mode_id: Option<u32> },
}

#[derive(Debug, Default, Clone)]
pub struct LobbyWatch {
    /// Bumped on every `.lq.Lobby.*` frame either way.
    pub lobby_seq: u64,
    pub last_lobby_at: Option<Instant>,
    /// Bumped on every successful login response.
    pub login_seq: u64,
    pub login_at: Option<Instant>,
    /// Own 4p / 3p rank ids (`AccountLevel.id`, e.g. 10301 = 4p 雀杰1).
    pub level4: Option<u32>,
    pub level3: Option<u32>,
    /// Bumped on every queue-related frame.
    pub queue_seq: u64,
    pub queue: QueueEvent,
    pub queue_at: Option<Instant>,
    /// Between a game socket's `authGame` response and that game's end
    /// (or termination). A reconnect into a running game re-enters it.
    pub in_game: bool,
    /// Bumped on every `authGame` response.
    pub game_seq: u64,
    /// Bumped on every game end; `last_end_terminated` says whether the
    /// last one was a termination rather than a finished game.
    pub game_end_seq: u64,
    pub last_end_terminated: bool,
    /// Last frame of game traffic (an action, or entering the game) — what
    /// a stall check measures from.
    pub last_game_frame_at: Option<Instant>,
    /// Bumped whenever a room's mode list asks for its queue counts; the
    /// mode ids it asked about say which room is open.
    pub room_view_seq: u64,
    pub room_view_modes: Vec<u32>,
    pub room_view_at: Option<Instant>,
    /// Bumped when a hand ends (`ActionHule` / `ActionNoTile` /
    /// `ActionLiuJu`), when a new one is dealt (`ActionNewRound`), and when
    /// the client confirms the result screens (`confirmNewRound`).
    pub round_end_seq: u64,
    pub new_round_seq: u64,
    pub round_confirm_seq: u64,
    /// Own account id, from the login response.
    pub account_id: Option<u64>,
    /// Bumped when the client asks for its own profile (see
    /// [`METHOD_FETCH_ACCOUNT_INFO`]).
    pub self_info_seq: u64,
}

pub type SharedLobbyWatch = Arc<RwLock<LobbyWatch>>;

pub fn new_shared() -> SharedLobbyWatch {
    Arc::new(RwLock::new(LobbyWatch::default()))
}

/// `"1:12"` → `Some(12)`; anything without a trailing number → `None`.
pub fn parse_match_sid(sid: &str) -> Option<u32> {
    sid.rsplit(':').next()?.trim().parse().ok()
}

/// `Error.code` of a response payload, `0` when absent (prost-reflect
/// omits default-valued fields, so a clean response has no `error` at all).
fn error_code(payload: &JsonValue) -> u32 {
    payload
        .pointer("/error/code")
        .and_then(JsonValue::as_u64)
        .unwrap_or(0) as u32
}

fn level_id(v: Option<&JsonValue>) -> Option<u32> {
    v?.get("id")?.as_u64().map(|n| n as u32)
}

impl LobbyWatch {
    /// Feed one decoded frame. Cheap for everything that is not lobby
    /// traffic: one prefix check.
    pub fn observe(&mut self, frame: LobbyFrame, method: &str, payload: &JsonValue) {
        let now = Instant::now();
        if method.starts_with(".lq.Lobby.") {
            self.lobby_seq += 1;
            self.last_lobby_at = Some(now);
        }
        match (frame, method) {
            // Every login flavour (`login`, `oauth2Login`, `emailLogin`)
            // answers with `ResLogin`, which carries the account.
            (LobbyFrame::Response, m) if m.starts_with(".lq.Lobby.") && m.ends_with("Login") => {
                if error_code(payload) != 0 {
                    return;
                }
                let Some(account) = payload.get("account") else {
                    return;
                };
                self.login_seq += 1;
                self.login_at = Some(now);
                if let Some(id) = account.get("account_id").and_then(JsonValue::as_u64) {
                    self.account_id = Some(id);
                }
                if let Some(id) = level_id(account.get("level")) {
                    self.level4 = Some(id);
                }
                if let Some(id) = level_id(account.get("level3")) {
                    self.level3 = Some(id);
                }
            }
            (LobbyFrame::Notify, METHOD_NOTIFY_ACCOUNT_LEVEL_CHANGE) => {
                self.note_level(level_id(payload.get("final")));
            }
            (LobbyFrame::Notify, METHOD_NOTIFY_GAME_FINISH_REWARD) => {
                self.note_level(level_id(payload.pointer("/level_change/final")));
            }
            (LobbyFrame::Request, METHOD_FETCH_ACCOUNT_INFO) => {
                if payload
                    .get("account_id")
                    .and_then(JsonValue::as_u64)
                    .unwrap_or(0)
                    == 0
                {
                    self.self_info_seq += 1;
                }
            }
            (LobbyFrame::Response, METHOD_FETCH_ACCOUNT_INFO) => {
                // Only our own profile carries our rank; another player's
                // (opened from a table) must not overwrite it.
                let account = payload.get("account");
                let id = account
                    .and_then(|a| a.get("account_id"))
                    .and_then(JsonValue::as_u64);
                if id.is_some() && id == self.account_id {
                    if let Some(a) = account {
                        self.note_level(level_id(a.get("level")));
                        self.note_level(level_id(a.get("level3")));
                    }
                }
            }
            (LobbyFrame::Request, METHOD_START_UNIFIED_MATCH) => {
                let mode_id = payload
                    .get("match_sid")
                    .and_then(JsonValue::as_str)
                    .and_then(parse_match_sid);
                self.set_queue(QueueEvent::Requested { mode_id }, now);
            }
            (LobbyFrame::Response, METHOD_START_UNIFIED_MATCH) => {
                let code = error_code(payload);
                if code == 0 {
                    self.set_queue(QueueEvent::Accepted, now);
                } else {
                    self.set_queue(QueueEvent::Rejected { code }, now);
                }
            }
            (LobbyFrame::Request, METHOD_FETCH_CURRENT_MATCH_INFO) => {
                self.room_view_seq += 1;
                self.room_view_at = Some(now);
                self.room_view_modes = payload
                    .get("mode_list")
                    .and_then(JsonValue::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_u64().map(|n| n as u32))
                            .collect()
                    })
                    .unwrap_or_default();
            }
            (LobbyFrame::Request, METHOD_CANCEL_UNIFIED_MATCH) => {
                self.set_queue(QueueEvent::Cancelled, now);
            }
            (LobbyFrame::Notify, METHOD_NOTIFY_MATCH_TIMEOUT)
            | (LobbyFrame::Notify, METHOD_NOTIFY_MATCH_FAILED) => {
                self.set_queue(QueueEvent::Dropped, now);
            }
            (LobbyFrame::Response, METHOD_AUTH_GAME) => {
                if error_code(payload) == 0 {
                    self.in_game = true;
                    self.game_seq += 1;
                    self.last_game_frame_at = Some(now);
                }
            }
            (LobbyFrame::Notify, METHOD_ACTION_PROTOTYPE) => {
                self.last_game_frame_at = Some(now);
                match payload.get("name").and_then(JsonValue::as_str) {
                    Some("ActionHule" | "ActionNoTile" | "ActionLiuJu") => self.round_end_seq += 1,
                    Some("ActionNewRound") => self.new_round_seq += 1,
                    _ => {}
                }
            }
            (LobbyFrame::Request, METHOD_CONFIRM_NEW_ROUND) => {
                self.round_confirm_seq += 1;
            }
            (LobbyFrame::Notify, METHOD_NOTIFY_GAME_END_RESULT)
            | (LobbyFrame::Notify, METHOD_NOTIFY_GAME_TERMINATE) => {
                // Only a game we saw start can end; a stray notify after a
                // reload must not count as a second game.
                if self.in_game {
                    self.in_game = false;
                    self.game_end_seq += 1;
                    self.last_end_terminated = method == METHOD_NOTIFY_GAME_TERMINATE;
                }
            }
            (LobbyFrame::Notify, METHOD_NOTIFY_MATCH_GAME_START) => {
                let mode_id = payload
                    .get("match_mode_id")
                    .and_then(JsonValue::as_u64)
                    .map(|n| n as u32);
                self.set_queue(QueueEvent::Matched { mode_id }, now);
            }
            _ => {}
        }
    }

    /// A rank id from any source; its leading digit says which ladder
    /// (1 = 4p, 2 = 3p).
    fn note_level(&mut self, id: Option<u32>) {
        match id {
            Some(id) if id / 10_000 == 1 => self.level4 = Some(id),
            Some(id) if id / 10_000 == 2 => self.level3 = Some(id),
            _ => {}
        }
    }

    fn set_queue(&mut self, ev: QueueEvent, now: Instant) {
        self.queue_seq += 1;
        self.queue = ev;
        self.queue_at = Some(now);
    }

    /// The rank id for the ladder `sanma` plays on.
    pub fn level_for(&self, sanma: bool) -> Option<u32> {
        if sanma {
            self.level3
        } else {
            self.level4
        }
    }
}

/// Rank tier of an `AccountLevel.id`: the hundreds digit
/// (1 初心, 2 雀士, 3 雀杰, 4 雀豪, 5 雀圣, 6/7 魂天).
pub fn rank_tier(level_id: u32) -> u32 {
    (level_id / 100) % 10
}

/// The highest ranked room (0 = Bronze … 4 = Throne) a rank admits.
pub fn best_room_for(level_id: u32) -> usize {
    match rank_tier(level_id) {
        0 | 1 => 0,
        2 => 1,
        3 => 2,
        4 => 3,
        _ => 4,
    }
}

/// The lowest rank tier each room admits, Bronze → Throne.
const ROOM_MIN_TIER: [u32; 5] = [1, 2, 3, 4, 5];

/// Whether a rank may enter a room at all (lower bound only; a room is
/// never refused for being too *low* a room by this check — the server has
/// the final word and its refusal stops full-auto with the code).
pub fn room_admits(room: usize, level_id: u32) -> bool {
    rank_tier(level_id) >= ROOM_MIN_TIER[room.min(4)]
}

/// The ranked mode id the server assigns a room × format, as it appears in
/// `match_sid` and `NotifyMatchGameStart.match_mode_id`. Used only to warn
/// when a queue landed on another mode than the one configured (a sign the
/// coordinates need recalibrating), never to decide anything.
pub fn expected_mode_id(room: usize, mode: crate::config::FullAutoMode) -> u32 {
    use crate::config::FullAutoMode::*;
    const FOUR: [[u32; 2]; 5] = [[2, 3], [5, 6], [8, 9], [11, 12], [15, 16]];
    const THREE: [[u32; 2]; 5] = [[17, 18], [19, 20], [21, 22], [23, 24], [25, 26]];
    let r = room.min(4);
    match mode {
        East4 => FOUR[r][0],
        South4 => FOUR[r][1],
        East3 => THREE[r][0],
        South3 => THREE[r][1],
    }
}

/// Which room (0 = Bronze … 4 = Throne) a `fetchCurrentMatchInfo`
/// `mode_list` belongs to, by its 4p East id. `None` if it names no
/// ranked room.
pub fn room_of_mode_list(modes: &[u32]) -> Option<usize> {
    (0..5).find(|&r| modes.contains(&expected_mode_id(r, crate::config::FullAutoMode::East4)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn login_response_records_both_ranks() {
        let mut w = LobbyWatch::default();
        w.observe(
            LobbyFrame::Response,
            ".lq.Lobby.oauth2Login",
            &json!({"account_id": 1, "account": {"level": {"id": 10302, "score": 5}, "level3": {"id": 20101}}}),
        );
        assert_eq!(w.login_seq, 1);
        assert_eq!(w.level4, Some(10302));
        assert_eq!(w.level3, Some(20101));
        assert_eq!(w.lobby_seq, 1);
    }

    #[test]
    fn failed_login_is_not_a_login() {
        let mut w = LobbyWatch::default();
        w.observe(
            LobbyFrame::Response,
            ".lq.Lobby.login",
            &json!({"error": {"code": 151}}),
        );
        assert_eq!(w.login_seq, 0);
        assert_eq!(w.lobby_seq, 1, "still lobby traffic");
    }

    #[test]
    fn queue_lifecycle() {
        let mut w = LobbyWatch::default();
        w.observe(
            LobbyFrame::Request,
            METHOD_START_UNIFIED_MATCH,
            &json!({"match_sid": "1:12", "client_version_string": "web-0"}),
        );
        assert_eq!(w.queue, QueueEvent::Requested { mode_id: Some(12) });
        w.observe(LobbyFrame::Response, METHOD_START_UNIFIED_MATCH, &json!({}));
        assert_eq!(w.queue, QueueEvent::Accepted);
        w.observe(
            LobbyFrame::Notify,
            METHOD_NOTIFY_MATCH_GAME_START,
            &json!({"match_mode_id": 12, "game_uuid": "x"}),
        );
        assert_eq!(w.queue, QueueEvent::Matched { mode_id: Some(12) });
        assert_eq!(w.queue_seq, 3);
    }

    #[test]
    fn refused_queue_keeps_the_code() {
        let mut w = LobbyWatch::default();
        w.observe(
            LobbyFrame::Response,
            METHOD_START_UNIFIED_MATCH,
            &json!({"error": {"code": 1208}}),
        );
        assert_eq!(w.queue, QueueEvent::Rejected { code: 1208 });
    }

    #[test]
    fn level_change_goes_to_its_ladder() {
        let mut w = LobbyWatch::default();
        w.observe(
            LobbyFrame::Notify,
            METHOD_NOTIFY_ACCOUNT_LEVEL_CHANGE,
            &json!({"origin": {"id": 10203}, "final": {"id": 10301}}),
        );
        w.observe(
            LobbyFrame::Notify,
            METHOD_NOTIFY_ACCOUNT_LEVEL_CHANGE,
            &json!({"final": {"id": 20201}}),
        );
        assert_eq!(w.level4, Some(10301));
        assert_eq!(w.level3, Some(20201));
    }

    #[test]
    fn game_traffic_is_ignored() {
        let mut w = LobbyWatch::default();
        w.observe(LobbyFrame::Notify, ".lq.ActionPrototype", &json!({}));
        assert_eq!(w.lobby_seq, 0);
        assert_eq!(w.queue_seq, 0);
    }

    #[test]
    fn a_game_counts_once() {
        let mut w = LobbyWatch::default();
        w.observe(
            LobbyFrame::Response,
            METHOD_AUTH_GAME,
            &json!({"seat_list": [1, 2, 3, 4]}),
        );
        assert!(w.in_game);
        assert_eq!(w.game_seq, 1);
        w.observe(LobbyFrame::Notify, METHOD_ACTION_PROTOTYPE, &json!({}));
        w.observe(
            LobbyFrame::Notify,
            METHOD_NOTIFY_GAME_END_RESULT,
            &json!({}),
        );
        w.observe(
            LobbyFrame::Notify,
            METHOD_NOTIFY_GAME_END_RESULT,
            &json!({}),
        );
        assert!(!w.in_game);
        assert_eq!(w.game_end_seq, 1);
        assert!(!w.last_end_terminated);
    }

    /// Shapes from a live game: each hand's end, the client's confirm
    /// about 15 s later, then the next deal.
    #[test]
    fn hand_end_confirm_and_next_deal() {
        let mut w = LobbyWatch::default();
        w.observe(
            LobbyFrame::Notify,
            METHOD_ACTION_PROTOTYPE,
            &json!({"name": "ActionHule", "data": {}}),
        );
        assert_eq!(w.round_end_seq, 1);
        w.observe(LobbyFrame::Request, METHOD_CONFIRM_NEW_ROUND, &json!({}));
        assert_eq!(w.round_confirm_seq, 1);
        w.observe(
            LobbyFrame::Notify,
            METHOD_ACTION_PROTOTYPE,
            &json!({"name": "ActionNewRound", "data": {}}),
        );
        assert_eq!(w.new_round_seq, 1);
        w.observe(
            LobbyFrame::Notify,
            METHOD_ACTION_PROTOTYPE,
            &json!({"name": "ActionNoTile", "data": {}}),
        );
        w.observe(
            LobbyFrame::Notify,
            METHOD_ACTION_PROTOTYPE,
            &json!({"name": "ActionLiuJu", "data": {}}),
        );
        w.observe(
            LobbyFrame::Notify,
            METHOD_ACTION_PROTOTYPE,
            &json!({"name": "ActionDiscardTile", "data": {}}),
        );
        assert_eq!(w.round_end_seq, 3);
    }

    #[test]
    fn rooms_by_rank() {
        assert_eq!(best_room_for(10101), 0);
        assert_eq!(best_room_for(10203), 1);
        assert_eq!(best_room_for(10301), 2);
        assert_eq!(best_room_for(10403), 3);
        assert_eq!(best_room_for(10501), 4);
        assert_eq!(best_room_for(10701), 4);
        assert!(room_admits(0, 10101));
        assert!(!room_admits(1, 10103));
        assert!(room_admits(2, 10402));
    }

    #[test]
    fn match_sid_parsing() {
        assert_eq!(parse_match_sid("1:12"), Some(12));
        assert_eq!(parse_match_sid("16"), Some(16));
        assert_eq!(parse_match_sid("x:"), None);
    }

    /// Shape taken from a live client: the Bronze room's mode list.
    #[test]
    fn room_view_names_the_room() {
        let mut w = LobbyWatch::default();
        w.observe(
            LobbyFrame::Request,
            METHOD_FETCH_CURRENT_MATCH_INFO,
            &json!({"mode_list": [2, 3, 17, 18]}),
        );
        assert_eq!(w.room_view_seq, 1);
        assert_eq!(room_of_mode_list(&w.room_view_modes), Some(0));
        assert_eq!(room_of_mode_list(&[15, 16, 25, 26]), Some(4));
        assert_eq!(room_of_mode_list(&[]), None);
    }

    /// Shapes taken from a live client's post-game traffic.
    #[test]
    fn post_game_rank_and_lobby_cue() {
        let mut w = LobbyWatch::default();
        w.observe(
            LobbyFrame::Response,
            ".lq.Lobby.oauth2Login",
            &json!({"account": {"account_id": 117, "level": {"id": 10101}, "level3": {"id": 20101}}}),
        );
        assert_eq!(w.account_id, Some(117));
        w.observe(
            LobbyFrame::Notify,
            METHOD_NOTIFY_GAME_FINISH_REWARD,
            &json!({"badges": [], "level_change": {"final": {"id": 10102, "score": 0}, "origin": {"id": 10101}}, "mode_id": 2}),
        );
        assert_eq!(w.level4, Some(10102));
        w.observe(
            LobbyFrame::Request,
            METHOD_FETCH_ACCOUNT_INFO,
            &json!({"account_id": 0}),
        );
        assert_eq!(w.self_info_seq, 1);
        // Someone else's profile: neither a cue nor a rank.
        w.observe(
            LobbyFrame::Request,
            METHOD_FETCH_ACCOUNT_INFO,
            &json!({"account_id": 999}),
        );
        w.observe(
            LobbyFrame::Response,
            METHOD_FETCH_ACCOUNT_INFO,
            &json!({"account": {"account_id": 999, "level": {"id": 10501}}}),
        );
        assert_eq!(w.self_info_seq, 1);
        assert_eq!(w.level4, Some(10102));
        w.observe(
            LobbyFrame::Response,
            METHOD_FETCH_ACCOUNT_INFO,
            &json!({"account": {"account_id": 117, "level": {"id": 10201}}}),
        );
        assert_eq!(w.level4, Some(10201));
    }

    #[test]
    fn mode_ids() {
        use crate::config::FullAutoMode;
        assert_eq!(expected_mode_id(4, FullAutoMode::South4), 16);
        assert_eq!(expected_mode_id(0, FullAutoMode::East4), 2);
        assert_eq!(expected_mode_id(3, FullAutoMode::South3), 24);
    }
}
