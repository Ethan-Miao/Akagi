//! The embedded game view: Mahjong Soul inside Akagi's own window.
//!
//! On Windows the game can run in a second WebView2 that is a child of the
//! main window, instead of in a separate Chromium the capture backend
//! launches. The view gets its own data directory (`webview/game/` next to
//! the exe — its own WebView2 environment, so it can take browser arguments
//! the main UI does not, and its login persists across runs) and a local
//! DevTools port (`--remote-debugging-port`). From there nothing is
//! different: `capture::chromium` attaches to that port with the same CDP
//! client, bridges and autoplay it uses for an external browser.
//!
//! **Layout is the frontend's.** It renders a placeholder where the game
//! should be and reports that rectangle (CSS px = logical px: the main
//! webview fills the client area at zoom 1) through `game_view_set_bounds`.
//! A native child webview always draws above the main one, so when the
//! placeholder is not on screen — another page, a dialog, the sidebar
//! flying out over it — the view is *parked* off-screen at the same size
//! rather than hidden: a hidden WebView2 stops rendering and its timers are
//! throttled, which would stall the game and autoplay with it. Clicks are
//! CDP input in page coordinates, so they land wherever the view sits.
//!
//! macOS and Linux webviews expose no DevTools protocol; there the capture
//! backend falls back to an external browser.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Position, Rect, Size,
    WebviewBuilder, WebviewUrl,
};
use tracing::{info, warn};

/// Label of the game webview.
pub const LABEL: &str = "game";
/// Where a parked view goes: far off the left edge, at its last size.
const PARK_X: f64 = -32000.0;
/// Size before the frontend has reported a placeholder (16:9).
const DEFAULT_SIZE: (f64, f64) = (1280.0, 720.0);

static APP: OnceLock<AppHandle> = OnceLock::new();
static BOUNDS: Mutex<Option<ViewBounds>> = Mutex::new(None);

/// The placeholder's rectangle in the main window, logical px, and whether
/// it is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ViewBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub visible: bool,
}

/// Emitted as `game-view` whenever the view opens or closes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameViewState {
    pub open: bool,
}

/// Whether this platform can embed the game (WebView2's DevTools port).
pub fn supported() -> bool {
    cfg!(windows)
}

/// Called once from `setup` with the app handle.
pub fn install(app: AppHandle) {
    let _ = APP.set(app);
}

fn app() -> Result<AppHandle> {
    APP.get()
        .cloned()
        .ok_or_else(|| anyhow!("embedded game view: app handle not installed"))
}

pub fn is_open() -> bool {
    APP.get().is_some_and(|a| a.get_webview(LABEL).is_some())
}

/// Where the view should be for the last reported bounds: on the
/// placeholder when it is visible, parked otherwise.
fn target_rect(bounds: Option<ViewBounds>) -> (f64, f64, f64, f64) {
    match bounds {
        Some(b) if b.width >= 2.0 && b.height >= 2.0 => {
            if b.visible {
                (b.x, b.y, b.width, b.height)
            } else {
                (PARK_X, b.y, b.width, b.height)
            }
        }
        _ => (PARK_X, 0.0, DEFAULT_SIZE.0, DEFAULT_SIZE.1),
    }
}

fn rect_of(r: (f64, f64, f64, f64)) -> Rect {
    Rect {
        position: Position::Logical(LogicalPosition::new(r.0, r.1)),
        size: Size::Logical(LogicalSize::new(r.2, r.3)),
    }
}

/// Open the game view at `url`, with WebView2 data in `data_dir` and the
/// DevTools endpoint on `127.0.0.1:port`. Replaces a view left open.
pub async fn open(url: &str, data_dir: PathBuf, port: u16) -> Result<()> {
    if !supported() {
        anyhow::bail!("the embedded game view needs Windows (WebView2)");
    }
    let app = app()?;
    close();
    let window = app
        .get_window("main")
        .ok_or_else(|| anyhow!("embedded game view: no main window"))?;
    let parsed: tauri::Url = url
        .parse()
        .with_context(|| format!("embedded game view: bad start URL {url:?}"))?;
    // Passing our own arguments replaces wry's defaults, so they are
    // repeated here. The rest keep the game drawing at full speed while the
    // window is covered (see `launch::KEEP_RUNNING_ARGS`).
    let args = format!(
        "--remote-debugging-port={port} \
         --disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,CalculateNativeWinOcclusion {}",
        crate::capture::chromium::launch::KEEP_RUNNING_ARGS.join(" ")
    );
    let builder = WebviewBuilder::new(LABEL, WebviewUrl::External(parsed))
        .data_directory(data_dir.clone())
        .additional_browser_args(&args);
    let r = target_rect(*BOUNDS.lock().unwrap());
    let rect = rect_of(r);
    // `add_child` hops to the main thread and blocks on the result.
    tokio::task::spawn_blocking(move || window.add_child(builder, rect.position, rect.size))
        .await
        .context("embedded game view: builder task panicked")?
        .context("embedded game view: creating the webview")?;
    info!(
        "embedded game view open: {url} (data {}, devtools port {port})",
        data_dir.display()
    );
    let _ = app.emit("game-view", GameViewState { open: true });
    Ok(())
}

/// Close the game view if it is open.
pub fn close() {
    let Ok(app) = app() else { return };
    if let Some(wv) = app.get_webview(LABEL) {
        if let Err(e) = wv.close() {
            warn!("embedded game view: close failed: {e}");
        }
        info!("embedded game view closed");
        let _ = app.emit("game-view", GameViewState { open: false });
    }
}

/// Record the placeholder's rectangle and move the view onto it (or park
/// it). Called by the frontend on every layout change.
pub fn set_bounds(bounds: ViewBounds) -> Result<()> {
    *BOUNDS.lock().unwrap() = Some(bounds);
    let Ok(app) = app() else { return Ok(()) };
    if let Some(wv) = app.get_webview(LABEL) {
        wv.set_bounds(rect_of(target_rect(Some(bounds))))
            .context("embedded game view: set_bounds")?;
    }
    Ok(())
}

/// A free local TCP port for the DevTools endpoint.
pub fn free_port() -> Result<u16> {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).context("finding a free local port")?;
    Ok(l.local_addr()?.port())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(visible: bool) -> ViewBounds {
        ViewBounds {
            x: 90.0,
            y: 40.0,
            width: 1000.0,
            height: 600.0,
            visible,
        }
    }

    #[test]
    fn visible_view_sits_on_the_placeholder() {
        assert_eq!(target_rect(Some(b(true))), (90.0, 40.0, 1000.0, 600.0));
    }

    /// Parked, not hidden: same size, off-screen.
    #[test]
    fn hidden_view_is_parked_at_its_size() {
        assert_eq!(target_rect(Some(b(false))), (PARK_X, 40.0, 1000.0, 600.0));
    }

    #[test]
    fn no_or_degenerate_bounds_park_at_default_size() {
        assert_eq!(target_rect(None), (PARK_X, 0.0, 1280.0, 720.0));
        let mut z = b(true);
        z.width = 0.0;
        assert_eq!(target_rect(Some(z)), (PARK_X, 0.0, 1280.0, 720.0));
    }

    #[test]
    fn free_port_is_nonzero() {
        assert_ne!(free_port().unwrap(), 0);
    }

    #[test]
    fn bounds_wire_shape() {
        let v: ViewBounds =
            serde_json::from_str(r#"{"x":1,"y":2,"width":3,"height":4,"visible":true}"#).unwrap();
        assert_eq!(
            v,
            ViewBounds {
                x: 1.0,
                y: 2.0,
                width: 3.0,
                height: 4.0,
                visible: true
            }
        );
    }
}
