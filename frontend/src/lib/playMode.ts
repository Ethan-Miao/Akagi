import { invoke } from '@/lib/tauri'
import i18n from '@/i18n'
import { useConfigStore } from '@/stores/configStore'
import { useFullAutoStore } from '@/stores/fullAutoStore'
import type { AppConfig, FullAutoStatus } from '@/types'

/**
 * How Akagi plays, as one switch:
 * - `manual`: recommendations only — autoplay off.
 * - `auto`: autoplay plays the hands; the player queues and closes the
 *   game-end screens.
 * - `full`: autoplay plus full-auto (queue → play → results → queue).
 */
export type PlayMode = 'manual' | 'auto' | 'full'

export function playModeOf(config: AppConfig | null, fullAutoRunning: boolean): PlayMode {
  if (fullAutoRunning) return 'full'
  return config?.autoplay.enabled ? 'auto' : 'manual'
}

async function setAutoplay(on: boolean) {
  const cur = useConfigStore.getState().config
  if (!cur || cur.autoplay.enabled === on) return
  const next: AppConfig = { ...cur, autoplay: { ...cur.autoplay, enabled: on } }
  await invoke('update_config', { newConfig: next })
  useConfigStore.getState().setConfig(next)
}

/** Switch modes. Throws the backend's message if a step fails. */
export async function setPlayMode(mode: PlayMode): Promise<void> {
  const setStatus = useFullAutoStore.getState().set
  const running = useFullAutoStore.getState().status.running
  if (mode !== 'full' && running) {
    setStatus(await invoke<FullAutoStatus>('full_auto_stop'))
  }
  if (mode === 'manual') {
    await setAutoplay(false)
    return
  }
  await setAutoplay(true)
  if (mode === 'full' && !running) {
    setStatus(await invoke<FullAutoStatus>('full_auto_start', { lang: i18n.language }))
  }
}
