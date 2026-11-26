import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { toast } from '@/components/ui/sonner'
import { useConfigStore } from '@/stores/configStore'
import { useFullAutoStore } from '@/stores/fullAutoStore'
import { playModeOf, setPlayMode, type PlayMode } from '@/lib/playMode'
import { cn } from '@/lib/utils'

const MODES: PlayMode[] = ['manual', 'auto', 'full']

/**
 * Manual / Auto / Full auto, always in reach (status bar). Full auto is
 * Mahjong Soul in Chromium capture only; when it stops on its own (batch
 * done, an error) the switch falls back to Auto, which is what is still on.
 */
export function PlayModeSwitch() {
  const { t } = useTranslation()
  const config = useConfigStore((s) => s.config)
  const status = useFullAutoStore((s) => s.status)
  const [busy, setBusy] = useState(false)
  if (!config) return null

  const mode = playModeOf(config, status.running)
  const fullBlocked =
    config.platform.kind !== 'Majsoul'
      ? t('play_mode.full_needs_majsoul')
      : config.capture.mode !== 'chromium'
        ? t('play_mode.full_needs_chromium')
        : null

  const choose = async (m: PlayMode) => {
    if (busy || m === mode) return
    setBusy(true)
    try {
      await setPlayMode(m)
    } catch (e) {
      toast.error(t('play_mode.switch_failed'), { description: String(e) })
    } finally {
      setBusy(false)
    }
  }

  const progress =
    status.max_games > 0 ? `${status.games_played}/${status.max_games}` : `${status.games_played}`

  return (
    <span className="flex items-center gap-2">
      <span
        role="radiogroup"
        aria-label={t('play_mode.label')}
        className="flex items-center rounded-md border border-border bg-background/60 p-0.5"
      >
        {MODES.map((m) => {
          const disabled = busy || (m === 'full' && !!fullBlocked)
          const active = m === mode
          return (
            <button
              key={m}
              type="button"
              role="radio"
              aria-checked={active}
              disabled={disabled && !active}
              title={m === 'full' && fullBlocked ? fullBlocked : t(`play_mode.${m}_hint`)}
              onClick={() => void choose(m)}
              className={cn(
                'rounded px-2 py-0.5 text-[11px] leading-4 transition-colors',
                active
                  ? m === 'full'
                    ? 'bg-emerald-600 text-white'
                    : m === 'auto'
                      ? 'bg-primary text-primary-foreground'
                      : 'bg-muted text-foreground'
                  : 'text-muted-foreground hover:text-foreground',
                disabled && !active && 'opacity-40 cursor-not-allowed',
              )}
            >
              {t(`play_mode.${m}`)}
            </button>
          )
        })}
      </span>
      {status.running && (
        <span className="hidden sm:inline text-muted-foreground" title={status.detail ?? undefined}>
          {t(`full_auto.phase.${status.phase}`)} · {t('full_auto.games')} {progress}
        </span>
      )}
    </span>
  )
}
