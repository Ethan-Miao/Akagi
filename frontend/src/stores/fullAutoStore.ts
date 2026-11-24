import { create } from 'zustand'
import type { FullAutoConfig, FullAutoStatus } from '@/types'

/** Latest `full-auto-status` from the backend (a `watch`: latest value only). */
type FullAutoStore = {
  status: FullAutoStatus
  set: (s: FullAutoStatus) => void
}

export const IDLE_FULL_AUTO: FullAutoStatus = {
  running: false,
  phase: 'idle',
  games_played: 0,
  max_games: 0,
  room: null,
  mode: 'south4',
  detail: null,
  stop_reason: null,
}

export const useFullAutoStore = create<FullAutoStore>((set) => ({
  status: IDLE_FULL_AUTO,
  set: (status) => set({ status }),
}))

/** Mirrors `FullAutoConfig::default()` — used when the loaded config was
 *  written by a build without the section. */
export const DEFAULT_FULL_AUTO: FullAutoConfig = {
  room: 'auto',
  mode: 'south4',
  max_games: 0,
  step_delay_ms: [700, 1600],
  result_delay_ms: [3500, 7000],
  result_gap_ms: [1800, 3500],
  result_presses: 3,
  rest_between_games_ms: [2000, 9000],
  queue_timeout_secs: 600,
  game_stall_secs: 300,
  coords: {
    ranked: [11.4, 2.65],
    rooms: [
      [11.64, 3.38],
      [11.64, 4.75],
      [11.64, 6.11],
      [11.64, 5.3],
      [11.64, 6.7],
    ],
    modes: [
      [11.64, 3.31],
      [11.64, 4.71],
      [11.64, 6.07],
      [11.64, 7.22],
    ],
    result_ok: [14.53, 8.24],
    back: [10.21, 2.22],
    room_scroll_from: 3,
    room_scroll_anchor: [11.64, 4.75],
  },
}
