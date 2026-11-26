import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Play, Square, Crosshair, RotateCcw, ChevronDown, ChevronRight } from 'lucide-react'
import { TileFrame } from '@/components/TileFrame'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { toast } from '@/components/ui/sonner'
import { invoke } from '@/lib/tauri'
import { setPlayMode } from '@/lib/playMode'
import { Switch } from '@/components/ui/switch'
import { useConfigStore } from '@/stores/configStore'
import { DEFAULT_FULL_AUTO, useFullAutoStore } from '@/stores/fullAutoStore'
import type { Breakpoint } from '@/tiles/defaults'
import type {
  AppConfig,
  FullAutoCalibrationTarget,
  FullAutoConfig,
  FullAutoMode,
  FullAutoRoom,
  FullAutoStatus,
} from '@/types'

const ROOMS: FullAutoRoom[] = ['auto', 'bronze', 'silver', 'gold', 'jade', 'throne']
const MODES: FullAutoMode[] = ['east4', 'south4', 'east3', 'south3']

const PHASE_COLOR: Record<string, string> = {
  idle: 'bg-zinc-500',
  waiting_lobby: 'bg-amber-500',
  queueing: 'bg-amber-500',
  queued: 'bg-sky-500',
  matched: 'bg-sky-500',
  in_game: 'bg-emerald-500',
  results: 'bg-emerald-500',
  resting: 'bg-emerald-500',
  reloading: 'bg-orange-500',
}

/** Calibration targets, in the order a queue presses them. */
const TARGETS: { key: string; target: FullAutoCalibrationTarget }[] = [
  { key: 'ranked', target: { kind: 'ranked' } },
  ...[0, 1, 2, 3, 4].map((i) => ({ key: `room_${i}`, target: { kind: 'room', index: i } as const })),
  { key: 'scroll_anchor', target: { kind: 'scroll_anchor' } },
  ...[0, 1, 2, 3].map((i) => ({ key: `mode_${i}`, target: { kind: 'mode', index: i } as const })),
  { key: 'back', target: { kind: 'back' } },
  { key: 'result_ok', target: { kind: 'result_ok' } },
  { key: 'round_ok', target: { kind: 'round_ok' } },
]

function fullAutoOf(cfg: AppConfig | null): FullAutoConfig {
  return { ...DEFAULT_FULL_AUTO, ...(cfg?.autoplay.full_auto ?? {}) }
}

export function FullAutoTile({ bp }: { bp: Breakpoint }) {
  const { t } = useTranslation()
  const status = useFullAutoStore((s) => s.status)
  const setStatus = useFullAutoStore((s) => s.set)
  const config = useConfigStore((s) => s.config)
  const setConfig = useConfigStore((s) => s.setConfig)
  const [busy, setBusy] = useState(false)
  const [showCalib, setShowCalib] = useState(false)
  const [calibKey, setCalibKey] = useState(TARGETS[0].key)
  const [recording, setRecording] = useState(false)

  const fa = fullAutoOf(config)
  const running = status.running

  // Settings are saved as they change, layered on the stored config, so
  // nothing capture-relevant differs and `update_config` restarts nothing.
  const save = async (patch: Partial<FullAutoConfig>) => {
    if (!config) return
    const next: AppConfig = {
      ...config,
      autoplay: { ...config.autoplay, full_auto: { ...fa, ...patch } },
    }
    try {
      await invoke('update_config', { newConfig: next })
      setConfig(next)
    } catch (e) {
      toast.error(t('full_auto.save_failed'), { description: String(e) })
    }
  }

  // Start = the status bar's "Full auto" (turns autoplay on too); Stop
  // leaves autoplay on, like switching to "Auto".
  const call = async (cmd: 'full_auto_start' | 'full_auto_stop') => {
    setBusy(true)
    try {
      if (cmd === 'full_auto_start') await setPlayMode('full')
      else setStatus(await invoke<FullAutoStatus>(cmd))
    } catch (e) {
      toast.error(String(e))
    } finally {
      setBusy(false)
    }
  }

  const calibrate = async () => {
    const entry = TARGETS.find((x) => x.key === calibKey)
    if (!entry) return
    setRecording(true)
    toast.info(t('full_auto.calib_recording'), {
      description: t(`full_auto.target.${entry.key}`),
      id: 'full-auto-calib',
    })
    try {
      const at = await invoke<[number, number]>('full_auto_calibrate', { target: entry.target })
      setConfig(await invoke<AppConfig>('get_config'))
      toast.success(t('full_auto.calib_saved'), {
        description: `${t(`full_auto.target.${entry.key}`)}: (${at[0]}, ${at[1]})`,
        id: 'full-auto-calib',
      })
    } catch (e) {
      toast.error(t('full_auto.calib_failed'), { description: String(e), id: 'full-auto-calib' })
    } finally {
      setRecording(false)
    }
  }

  const resetCoords = async () => {
    try {
      await invoke('full_auto_reset_coords')
      setConfig(await invoke<AppConfig>('get_config'))
      toast.success(t('full_auto.coords_reset'))
    } catch (e) {
      toast.error(String(e))
    }
  }

  const dot = PHASE_COLOR[status.phase] ?? 'bg-zinc-500'
  const progress =
    status.max_games > 0
      ? `${status.games_played} / ${status.max_games}`
      : `${status.games_played}`
  const stop = status.stop_reason
  const stopText = stop
    ? t(`full_auto.stop.${stop.code}`, { defaultValue: stop.message })
    : null

  return (
    <TileFrame id="full-auto" title={t('tile.full_auto')} bp={bp} contentClassName="flex flex-col gap-2 text-sm">
      <div className="flex items-center gap-2">
        <span className={`h-2 w-2 rounded-full ${dot}`} />
        <span className="font-medium">{t(`full_auto.phase.${status.phase}`)}</span>
        <span className="ml-auto text-xs text-muted-foreground">
          {t('full_auto.games')}: <span className="font-mono">{progress}</span>
        </span>
      </div>
      {running && status.room && (
        <div className="text-xs text-muted-foreground truncate" title={status.detail ?? undefined}>
          {t(`full_auto.room.${status.room}`)} · {t(`full_auto.mode.${status.mode}`)}
          {status.detail ? ` — ${status.detail}` : ''}
        </div>
      )}
      {!running && stopText && (
        <div
          className={`text-[11px] ${stop && ['user', 'batch_done'].includes(stop.code) ? 'text-muted-foreground' : 'text-amber-400'}`}
          title={stop?.message}
        >
          {stopText}
        </div>
      )}

      <div className="grid grid-cols-3 gap-1.5" onPointerDown={(e) => e.stopPropagation()}>
        <Select value={fa.room} disabled={running} onValueChange={(v) => void save({ room: v as FullAutoRoom })}>
          <SelectTrigger className="w-full h-8 text-xs">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {ROOMS.map((r) => (
              <SelectItem key={r} value={r}>
                {t(`full_auto.room.${r}`)}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Select value={fa.mode} disabled={running} onValueChange={(v) => void save({ mode: v as FullAutoMode })}>
          <SelectTrigger className="w-full h-8 text-xs">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {MODES.map((m) => (
              <SelectItem key={m} value={m}>
                {t(`full_auto.mode.${m}`)}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Input
          type="number"
          min={0}
          className="h-8 text-xs"
          disabled={running}
          title={t('full_auto.max_games_hint')}
          placeholder={t('full_auto.max_games')}
          value={fa.max_games === 0 ? '' : fa.max_games}
          onChange={(e) => {
            const n = Math.max(0, Math.floor(Number(e.target.value) || 0))
            void save({ max_games: n })
          }}
        />
      </div>

      <label
        className="flex items-center gap-2 text-xs text-muted-foreground"
        title={t('full_auto.curtain_hint')}
        onPointerDown={(e) => e.stopPropagation()}
      >
        <Switch
          size="sm"
          checked={fa.curtain ?? true}
          onCheckedChange={(v) => void save({ curtain: v })}
        />
        {t('full_auto.curtain')}
      </label>

      <div className="flex gap-1.5">
        <Button
          variant="default"
          size="sm"
          className="flex-1 gap-1.5"
          onPointerDown={(e) => e.stopPropagation()}
          onClick={() => call('full_auto_start')}
          disabled={busy || running || !config}
        >
          <Play className="h-3.5 w-3.5" />
          {t('common.start')}
        </Button>
        <Button
          variant="destructive"
          size="sm"
          className="flex-1 gap-1.5"
          onPointerDown={(e) => e.stopPropagation()}
          onClick={() => call('full_auto_stop')}
          disabled={busy || !running}
        >
          <Square className="h-3.5 w-3.5" />
          {t('common.stop')}
        </Button>
      </div>

      <button
        type="button"
        className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground self-start"
        onPointerDown={(e) => e.stopPropagation()}
        onClick={() => setShowCalib((v) => !v)}
      >
        {showCalib ? <ChevronDown className="h-3 w-3" /> : <ChevronRight className="h-3 w-3" />}
        {t('full_auto.calibration')}
      </button>
      {showCalib && (
        <div className="flex flex-col gap-1.5" onPointerDown={(e) => e.stopPropagation()}>
          <p className="text-[11px] text-muted-foreground">{t('full_auto.calib_hint')}</p>
          <div className="flex gap-1.5">
            <Select value={calibKey} onValueChange={setCalibKey} disabled={recording || running}>
              <SelectTrigger className="flex-1 h-8 text-xs">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {TARGETS.map((x) => (
                  <SelectItem key={x.key} value={x.key}>
                    {t(`full_auto.target.${x.key}`)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Button size="sm" variant="outline" className="gap-1" disabled={recording || running} onClick={calibrate}>
              <Crosshair className="h-3.5 w-3.5" />
              {recording ? t('full_auto.calib_waiting') : t('full_auto.calib_record')}
            </Button>
            <Button
              size="sm"
              variant="ghost"
              title={t('full_auto.coords_reset')}
              disabled={recording || running}
              onClick={resetCoords}
            >
              <RotateCcw className="h-3.5 w-3.5" />
            </Button>
          </div>
        </div>
      )}
    </TileFrame>
  )
}
