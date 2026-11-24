import { create } from 'zustand'

// The embedded game view (Windows): whether it is open (`game-view` event /
// `game_view_state`), and the width of the panel beside it, a UI-only
// preference kept in localStorage.
const PANEL_KEY = 'akagi.gameView.panelWidth'
export const PANEL_MIN = 300
export const PANEL_MAX = 900
export const PANEL_DEFAULT = 420

function clampPanel(v: number): number {
  if (!Number.isFinite(v)) return PANEL_DEFAULT
  return Math.round(Math.min(PANEL_MAX, Math.max(PANEL_MIN, v)))
}

function loadPanel(): number {
  try {
    const raw = localStorage.getItem(PANEL_KEY)
    return raw ? clampPanel(parseFloat(raw)) : PANEL_DEFAULT
  } catch {
    return PANEL_DEFAULT
  }
}

type GameViewStore = {
  open: boolean
  setOpen: (open: boolean) => void
  panelWidth: number
  setPanelWidth: (px: number) => void
}

export const useGameViewStore = create<GameViewStore>((set) => ({
  open: false,
  setOpen: (open) => set({ open }),
  panelWidth: loadPanel(),
  setPanelWidth: (px) => {
    const panelWidth = clampPanel(px)
    try {
      localStorage.setItem(PANEL_KEY, String(panelWidth))
    } catch {
      /* quota — ignore */
    }
    set({ panelWidth })
  },
}))
