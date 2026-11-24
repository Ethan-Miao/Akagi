//! Chromium capture backend.
//!
//! Launches a Chromium-family browser with `--user-data-dir` (so it
//! doesn't collide with the user's existing Chrome) and a remote debugging port,
//! then connects to it via the Chrome DevTools Protocol and intercepts
//! `Network.webSocketFrameReceived/Sent` for binary frames. Frames are
//! routed into the platform [`crate::bridge::Bridge`] just as the
//! hudsucker backend does.
//!
//! No CA cert. No system proxy. The user just plays the game in the
//! Akagi-spawned browser window.

pub mod cdp;
pub mod cft;
pub mod detect;
pub mod launch;
pub mod profile;

use super::{CaptureBackend, CaptureCtx, CaptureDescriptor, CaptureKind, ShutdownToken};
use crate::capture::flow::FlowBridges;
use crate::config::ChromiumConfig;
use anyhow::{Context, Result};
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{info, warn};

pub struct ChromiumBackend {
    cfg: ChromiumConfig,
}

impl ChromiumBackend {
    pub fn new(cfg: ChromiumConfig) -> Self {
        Self { cfg }
    }

    /// Resolve the chrome executable. Resolution order:
    /// 1. Explicit `cfg.executable` if set (must exist).
    /// 2. If `force_cft = false`: first auto-detected system browser.
    /// 3. Installed Chrome-for-Testing (latest version, or pinned via
    ///    `cfg.cft_channel` if a literal version is installed).
    /// 4. Error pointing the user at the Settings UI to install CfT.
    fn resolve_executable(&self) -> Result<PathBuf> {
        if !self.cfg.executable.is_empty() {
            let p = PathBuf::from(&self.cfg.executable);
            if !p.exists() {
                anyhow::bail!(
                    "configured chromium executable does not exist: {}",
                    p.display()
                );
            }
            return Ok(p);
        }

        if !self.cfg.force_cft {
            if let Some(b) = detect::detect_system_browsers().into_iter().next() {
                return Ok(b.path);
            }
        }

        let pinned = cft::Channel::parse(&self.cfg.cft_channel);
        if let Some(exe) = cft::installed_executable(&pinned) {
            return Ok(exe);
        }

        anyhow::bail!(
            "no Chromium-family browser detected and no Chrome-for-Testing installed. \
             Open Settings → Capture and click Download to install Chrome for Testing, \
             or set capture.chromium.executable explicitly."
        )
    }
}

impl ChromiumBackend {
    /// Bridges + the CDP loop against a DevTools endpoint — the half of a
    /// run that does not care who started the browser.
    async fn run_cdp(&self, ctx: &CaptureCtx, cdp_endpoint: &str) -> Result<()> {
        let mut hooks = ctx
            .autoplay
            .as_ref()
            .map(|a| crate::bridge::BridgeHooks {
                time_budget: Some(a.time_budget.clone()),
                input_watch: Some(a.input_watch.clone()),
                tenhou_state: Some(a.tenhou_state.clone()),
                // Chromium has no injection relay; Riichi City autoplay
                // only runs on the MITM path.
                riichi_inject: None,
                notify: None,
                tenhou_journal: None,
                lobby_watch: Some(a.lobby.clone()),
            })
            .unwrap_or_default();
        hooks.notify = Some(ctx.notify_bus.clone());
        hooks.tenhou_journal = Some(crate::bridge::tenhou::journal::new_shared());
        let bridges = Arc::new(FlowBridges::<cdp::FlowKey>::new(
            ctx.session.clone(),
            ctx.platform,
            hooks,
        ));
        cdp::run(
            cdp_endpoint,
            bridges,
            ctx.mjai_bus.clone(),
            ctx.session.inspector(),
            ctx.autoplay.clone(),
            ctx.http.clone(),
            ctx.notify_bus.clone(),
            self.cfg.block_telemetry,
        )
        .await
    }

    /// The embedded path (Windows): open the game in a child WebView2 of
    /// the main window with a DevTools port, attach to it, close it on the
    /// way out. See `crate::embed`.
    async fn run_embedded(self: Box<Self>, ctx: CaptureCtx, shutdown: ShutdownToken) -> Result<()> {
        let data_dir = crate::util::resolve_dir(std::path::Path::new("./webview/game"));
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("creating game webview dir {}", data_dir.display()))?;
        let port = crate::embed::free_port()?;
        let url = if self.cfg.start_url.is_empty() {
            ChromiumConfig::default().start_url
        } else {
            self.cfg.start_url.clone()
        };
        info!(
            "chromium backend starting (embedded): {url} data={} port={port}",
            data_dir.display()
        );
        crate::embed::open(&url, data_dir.clone(), port)
            .await
            .context("opening the embedded game view")?;

        let result = async {
            let cdp_endpoint = launch::wait_for_devtools_endpoint(&data_dir, Some(port))
                .await
                .context("reading the game view's CDP endpoint (WebView2 failed to start?)")?;
            info!("embedded CDP endpoint: {cdp_endpoint}");
            let cdp_fut = self.run_cdp(&ctx, &cdp_endpoint);
            tokio::select! {
                biased;
                _ = shutdown.wait() => {
                    info!("chromium backend (embedded): shutdown requested");
                    Ok(())
                }
                r = cdp_fut => {
                    match &r {
                        Ok(()) => info!("chromium backend (embedded): CDP loop exited cleanly"),
                        Err(e) => warn!("chromium backend (embedded): CDP loop error: {e:#}"),
                    }
                    r
                }
            }
        }
        .await;
        crate::embed::close();
        result
    }
}

#[async_trait]
impl CaptureBackend for ChromiumBackend {
    async fn run(self: Box<Self>, ctx: CaptureCtx, shutdown: ShutdownToken) -> Result<()> {
        if self.cfg.embedded {
            if crate::embed::supported() {
                return self.run_embedded(ctx, shutdown).await;
            }
            warn!("embedded game view is Windows-only; launching a browser instead");
        }
        let exe = self
            .resolve_executable()
            .context("resolving chromium executable")?;
        let profile_dir = profile::resolve_profile_dir(&self.cfg.user_data_dir)?;
        std::fs::create_dir_all(&profile_dir)
            .with_context(|| format!("creating chromium profile dir {}", profile_dir.display()))?;
        // A browser we previously launched may still be running with this
        // profile (e.g. the user closed Akagi but left Chrome open). Spawning a
        // second `--user-data-dir` instance while the first is alive only opens
        // a duplicate tab in it and then exits, leaving capture with no DevTools
        // endpoint. (Two Akagi instances against one profile is unsupported.)
        // `reclaim_singleton` finds and terminates that browser before we
        // relaunch — via the `SingletonLock` symlink on Unix, or, on Windows
        // (where Chrome writes no such file), by matching its command-line
        // `--user-data-dir`. It blocks, so run it off the async runtime.
        {
            let pd = profile_dir.clone();
            tokio::task::spawn_blocking(move || profile::reclaim_singleton(&pd))
                .await
                .context("reclaim-singleton task panicked")?
                .with_context(|| {
                    format!("reclaiming chromium profile {}", profile_dir.display())
                })?;
        }

        info!(
            "chromium backend starting: exe={} profile={}",
            exe.display(),
            profile_dir.display()
        );

        let launched =
            launch::spawn(&exe, &profile_dir, &self.cfg).context("launching chromium")?;
        let mut child = launched.child;

        let cdp_endpoint =
            launch::wait_for_devtools_endpoint(&profile_dir, launched.remote_debugging_port)
                .await
                .context("reading chromium CDP endpoint (chromium failed to start?)")?;
        info!("chromium CDP endpoint: {cdp_endpoint}");

        let mut cdp_fut = Box::pin(self.run_cdp(&ctx, &cdp_endpoint));
        let shutdown_fut = shutdown.wait();
        tokio::pin!(shutdown_fut);

        // Race shutdown, the CDP loop, and the spawned process's exit. The
        // shutdown and CDP arms are terminal. The child-exit arm may not be:
        // after a launcher handoff (see below) we merely disable that arm via
        // its `if !child_exited` precondition and loop, so shutdown and CDP
        // keep racing — one set of arms instead of a duplicated inner select.
        let mut child_exited = false;
        let result = loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_fut => {
                    info!("chromium backend: shutdown requested");
                    break Ok(());
                }
                r = &mut cdp_fut => {
                    match &r {
                        Ok(()) => info!("chromium backend: CDP loop exited cleanly"),
                        Err(e) => warn!("chromium backend: CDP loop error: {e:#}"),
                    }
                    break r;
                }
                status = child.wait(), if !child_exited => {
                    child_exited = true;
                    // A clean exit of the process we spawned is not necessarily
                    // the browser dying: Edge (and Chrome in some setups) may
                    // relaunch itself with the same arguments and exit the
                    // original process with code 0 — a launcher handoff. The
                    // relaunched browser keeps serving our CDP endpoint, so probe
                    // it before declaring the browser dead.
                    let handoff = matches!(&status, Ok(s) if s.success())
                        && match launch::endpoint_port(&cdp_endpoint) {
                            Some(port) => launch::devtools_http_alive(port).await,
                            None => false,
                        };
                    if handoff {
                        info!(
                            "chromium backend: spawned process exited but the CDP \
                             endpoint is still alive (launcher handoff); continuing"
                        );
                        // Loop again with the child arm disabled.
                    } else {
                        break match status {
                            Ok(s) => {
                                warn!("chromium backend: browser exited (status {s})");
                                Err(anyhow::anyhow!("browser exited unexpectedly: {s}"))
                            }
                            Err(e) => Err(anyhow::anyhow!("child wait error: {e}")),
                        };
                    }
                }
            }
        };

        // Best-effort shutdown of the process we spawned. After a launcher
        // handoff this only reaps the (already dead) launcher and the real
        // browser is deliberately left running: the user may be mid-match,
        // and a surviving browser keeps the game connected even when Akagi
        // itself stops or crashes. The next capture start reclaims it (see
        // `reclaim_singleton` above) before relaunching.
        launch::terminate(&mut child).await;
        result
    }

    fn descriptor(&self) -> CaptureDescriptor {
        let label = if self.cfg.embedded && crate::embed::supported() {
            "embedded".to_string()
        } else if self.cfg.executable.is_empty() {
            "auto-detect".to_string()
        } else {
            self.cfg.executable.clone()
        };
        CaptureDescriptor {
            kind: CaptureKind::Chromium,
            label,
        }
    }
}
