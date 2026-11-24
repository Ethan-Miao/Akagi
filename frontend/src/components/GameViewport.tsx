import { useEffect, useRef } from 'react'
import { useTranslation } from 'react-i18next'
import { invoke } from '@/lib/tauri'
import { useSidebar } from '@/hooks/useSidebar'

type Bounds = { x: number; y: number; width: number; height: number; visible: boolean }

/**
 * Placeholder for the embedded game view (Windows). The game itself is a
 * native WebView2 the backend lays over this element: on every layout
 * change this reports its rectangle (`game_view_set_bounds`).
 *
 * A native child webview draws above everything in the main webview, so
 * whenever something of ours should be on top of the game — a dialog, a
 * dropdown, the sidebar flying out over it — the view is reported as not
 * visible and the backend parks it off-screen (not hidden: a hidden
 * WebView2 stops rendering, which would stall the game and autoplay).
 * Unmounting (another page) parks it too.
 */
export function GameViewport() {
  const { t } = useTranslation()
  const ref = useRef<HTMLDivElement>(null)
  const sidebarHover = useSidebar((s) => s.isHover && !s.isOpen && s.settings.isHoverOpen)

  useEffect(() => {
    const el = ref.current
    if (!el) return
    let last = ''
    let raf = 0

    const overlayOpen = () =>
      document.querySelector(
        '[role="dialog"],[role="alertdialog"],[role="menu"],[role="listbox"],[data-radix-popper-content-wrapper]',
      ) !== null

    const report = () => {
      cancelAnimationFrame(raf)
      raf = requestAnimationFrame(() => {
        const r = el.getBoundingClientRect()
        const b: Bounds = {
          x: Math.round(r.left),
          y: Math.round(r.top),
          width: Math.round(r.width),
          height: Math.round(r.height),
          visible: !overlayOpen() && !sidebarHover && document.visibilityState === 'visible',
        }
        const key = JSON.stringify(b)
        if (key === last) return
        last = key
        invoke('game_view_set_bounds', { bounds: b }).catch(() => {})
      })
    }

    report()
    const ro = new ResizeObserver(report)
    ro.observe(el)
    ro.observe(document.body)
    // Dialogs and popovers are portalled into <body>.
    const mo = new MutationObserver(report)
    mo.observe(document.body, { childList: true, subtree: false })
    window.addEventListener('resize', report)
    document.addEventListener('visibilitychange', report)
    return () => {
      cancelAnimationFrame(raf)
      ro.disconnect()
      mo.disconnect()
      window.removeEventListener('resize', report)
      document.removeEventListener('visibilitychange', report)
      const r = el.getBoundingClientRect()
      invoke('game_view_set_bounds', {
        bounds: { x: r.left, y: r.top, width: r.width, height: r.height, visible: false },
      }).catch(() => {})
    }
  }, [sidebarHover])

  return (
    <div
      ref={ref}
      className="relative flex-1 min-w-0 min-h-0 bg-black/80 flex items-center justify-center text-sm text-muted-foreground"
    >
      {t('game_view.loading')}
    </div>
  )
}
