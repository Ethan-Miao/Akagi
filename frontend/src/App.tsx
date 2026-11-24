import { useEffect, useRef } from 'react'
import { Outlet, useLocation } from 'react-router-dom'
import { Sidebar } from '@/components/sidebar/Sidebar'
import { NarrowTopBar } from '@/components/sidebar/NarrowTopBar'
import { Statusbar } from '@/components/Statusbar'
import { ErrorBoundary } from '@/components/ErrorBoundary'
import { Toaster } from '@/components/ui/sonner'
import { UpdateNotifier } from '@/components/UpdateNotifier'
import { AnnouncementsDialog } from '@/components/AnnouncementsDialog'
import { DelayScriptUpdateDialog } from '@/components/DelayScriptUpdateDialog'
import { InstallBlockingOverlay } from '@/components/InstallBlockingOverlay'
import { useTauriBridge } from '@/hooks/useTauriBridge'
import { useSidebar } from '@/hooks/useSidebar'
import { useUiPrefsStore } from '@/stores/uiPrefsStore'
import { cn } from '@/lib/utils'
import { GameViewport } from '@/components/GameViewport'
import { useGameViewStore } from '@/stores/gameViewStore'

export default function App() {
  useTauriBridge()
  const location = useLocation()
  const scale = useUiPrefsStore((s) => s.scale)
  const isOpen = useSidebar((s) => s.isOpen)
  const settings = useSidebar((s) => s.settings)
  // Embedded game view (Windows): on the dashboard the game takes the left
  // and the page becomes a panel on the right; on other pages the page gets
  // the full width and the game is parked off-screen (still running).
  const gameOpen = useGameViewStore((s) => s.open)
  const panelWidth = useGameViewStore((s) => s.panelWidth)
  const split = gameOpen && location.pathname === '/game'

  // Apply UI scale by adjusting :root font-size — the design system is
  // rem-based so this scales typography, spacing, icons, sidebar width
  // (w-[18rem] / w-[5.625rem]), and Radix portals uniformly.
  useEffect(() => {
    document.documentElement.style.fontSize = `${16 * scale}px`
    return () => {
      document.documentElement.style.fontSize = ''
    }
  }, [scale])

  return (
    <>
      <Sidebar />
      <main
        className={cn(
          'h-screen flex flex-col min-w-0 overflow-hidden bg-background text-foreground transition-[margin-left] ease-in-out duration-300',
          // Margin tracks the *pinned* width only — hover-open expands the
          // sidebar over main content (z-20) without shifting layout, so
          // width-sensitive widgets like the GameDashboard's react-grid-layout
          // don't re-flow every time the cursor brushes the sidebar.
          settings.disabled
            ? 'lg:ml-0'
            : isOpen
              ? 'lg:ml-[18rem]'
              : 'lg:ml-[5.625rem]',
        )}
      >
        {/* Below `lg` the sidebar is an off-screen drawer, so this is the
            only navigation entry point. Renders null when docked. */}
        <NarrowTopBar />
        <div className="flex-1 min-h-0 flex">
          {split && <GameViewport />}
          {split && <PanelDivider />}
          <div
            className={cn('overflow-auto', split ? 'shrink-0' : 'flex-1')}
            style={split ? { width: panelWidth } : undefined}
          >
            {/* Keyed by route so a crash on one page doesn't poison the others. */}
            <ErrorBoundary key={location.pathname}>
              <Outlet />
            </ErrorBoundary>
          </div>
        </div>
        <Statusbar />
      </main>
      <Toaster />
      <UpdateNotifier />
      <AnnouncementsDialog />
      <DelayScriptUpdateDialog />
      <InstallBlockingOverlay />
    </>
  )
}

/** Drag handle between the embedded game and the panel. */
function PanelDivider() {
  const setPanelWidth = useGameViewStore((s) => s.setPanelWidth)
  const drag = useRef<{ x: number; w: number } | null>(null)
  return (
    <div
      role="separator"
      aria-orientation="vertical"
      className="w-1.5 shrink-0 cursor-col-resize bg-border/60 hover:bg-primary/40 active:bg-primary/60"
      onPointerDown={(e) => {
        e.currentTarget.setPointerCapture(e.pointerId)
        drag.current = { x: e.clientX, w: useGameViewStore.getState().panelWidth }
      }}
      onPointerMove={(e) => {
        if (!drag.current) return
        setPanelWidth(drag.current.w - (e.clientX - drag.current.x))
      }}
      onPointerUp={() => {
        drag.current = null
      }}
    />
  )
}
