//! The full-auto curtain: while full-auto runs, a transparent element
//! covers the game page and swallows the player's own mouse and keys.
//!
//! A stray click while full-auto drives the lobby can open a menu the next
//! press then lands in; one during a hand can discard a tile. The curtain is
//! a fixed, full-viewport element on top of the canvas that catches those
//! (and stops them reaching the client's window-level listeners), with a
//! small badge saying why the game ignores the mouse. Switching to Auto or
//! Manual lifts it.
//!
//! Synthesised input is hit-tested like real input, so the curtain would
//! catch autoplay's presses too. Every press, move and wheel in
//! `cdp_input` therefore opens it for the duration of that one event
//! sequence (`__akagiCurtain.arm()` / `.done(ms)`, reference-counted, with
//! a safety timeout that closes it again if a `done` is ever lost).
//!
//! It lives in the page, so a reload removes it; the task below re-applies
//! it every couple of seconds while it should be up, which also covers a
//! page swapped for another tab.

use crate::autoplay::cdp_input::CURTAIN_UP;
use crate::autoplay::context::AutoplayContext;
use crate::config::{AppConfig, Platform};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, info};

const REAPPLY_EVERY: Duration = Duration::from_secs(2);

/// The badge text, in the dashboard's language.
pub fn badge_text(lang: &str) -> &'static str {
    let l = lang.to_ascii_lowercase();
    if l.starts_with("zh-tw") || l.starts_with("zh-hk") || l.starts_with("zh-hant") {
        "全自動運行中 · 已屏蔽滑鼠操作 · 在 Akagi 切換到「自動」或「手動」即可接管"
    } else if l.starts_with("zh") {
        "全自动运行中 · 已屏蔽鼠标操作 · 在 Akagi 切换到「自动」或「手动」即可接管"
    } else if l.starts_with("ja") {
        "フルオート実行中 · マウス操作は無効 · Akagi で「自動」か「手動」に切り替えると操作できます"
    } else {
        "Full auto running · mouse input blocked · switch Akagi to Auto or Manual to take over"
    }
}

/// Script that puts the curtain up (idempotently, updating the badge) or
/// takes it down. Returns `"on"` / `"off"`.
pub fn script(up: bool, text: &str) -> String {
    let text = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into());
    if !up {
        return "(()=>{const c=window.__akagiCurtain;if(c)c.remove();return 'off'})()".into();
    }
    format!(
        r#"(()=>{{
const T={text};
let c=window.__akagiCurtain;
if(c&&c.el&&c.el.isConnected){{c.setText(T);return 'on';}}
if(c)c.remove();
const el=document.createElement('div');
el.id='__akagi_curtain';
el.setAttribute('aria-hidden','true');
el.style.cssText='position:fixed;left:0;top:0;right:0;bottom:0;z-index:2147483647;background:transparent;cursor:not-allowed;pointer-events:auto;';
const b=document.createElement('div');
b.style.cssText='position:absolute;top:6px;left:50%;transform:translateX(-50%);max-width:92%;padding:3px 12px;border-radius:999px;background:rgba(15,23,42,.72);color:#f8fafc;font:12px/1.6 system-ui,-apple-system,"Segoe UI","Microsoft YaHei",sans-serif;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;pointer-events:none;user-select:none;';
b.textContent=T;
el.appendChild(b);
const stop=e=>{{e.stopPropagation();if(e.cancelable)e.preventDefault();}};
for(const t of ['pointerdown','pointerup','pointermove','pointerover','pointerout','pointerenter','pointerleave','pointercancel','mousedown','mouseup','mousemove','mouseover','mouseout','click','dblclick','auxclick','contextmenu','wheel','touchstart','touchmove','touchend','dragstart'])
  el.addEventListener(t,stop,{{passive:false}});
const keys=e=>{{if(document.getElementById('__akagi_curtain')){{e.stopImmediatePropagation();if(e.cancelable)e.preventDefault();}}}};
for(const t of ['keydown','keyup','keypress'])window.addEventListener(t,keys,true);
(document.body||document.documentElement).appendChild(el);
let n=0,safety=null;
const shut=()=>{{n=0;el.style.pointerEvents='auto';}};
c={{el,
 arm(){{n++;el.style.pointerEvents='none';clearTimeout(safety);safety=setTimeout(shut,8000);}},
 done(ms){{setTimeout(()=>{{n=Math.max(0,n-1);if(n===0){{el.style.pointerEvents='auto';clearTimeout(safety);}}}},ms||150);}},
 setText(t){{b.textContent=t;}},
 remove(){{clearTimeout(safety);for(const t of ['keydown','keyup','keypress'])window.removeEventListener(t,keys,true);el.remove();if(window.__akagiCurtain===c)delete window.__akagiCurtain;}}}};
window.__akagiCurtain=c;
return 'on';
}})()"#
    )
}

/// Long-lived task: keeps the curtain on the game page exactly while
/// full-auto runs (and `autoplay.full_auto.curtain` is on).
pub async fn run_curtain(cfg: Arc<RwLock<AppConfig>>, ctx: Arc<AutoplayContext>) {
    let mut run_rx = ctx.full_auto.subscribe_run();
    let mut up = false;
    loop {
        let want = {
            let cfg = cfg.read().await;
            ctx.full_auto.is_running()
                && cfg.autoplay.full_auto.curtain
                && cfg.platform.kind == Platform::Majsoul
        };
        let page = ctx.page.read().await.as_ref().cloned();
        if want {
            // Mark it first: a press racing the install must arm it.
            CURTAIN_UP.store(true, Ordering::Relaxed);
            if let Some(page) = &page {
                let text = badge_text(&ctx.full_auto.lang());
                match page.evaluate(script(true, text)).await {
                    Ok(_) => {
                        if !up {
                            info!("full-auto: curtain up over the game page");
                        }
                        up = true;
                    }
                    Err(e) => debug!("full-auto: curtain: {e:#}"),
                }
            }
        } else if up || CURTAIN_UP.load(Ordering::Relaxed) {
            if let Some(page) = &page {
                let _ = page.evaluate(script(false, "")).await;
            }
            CURTAIN_UP.store(false, Ordering::Relaxed);
            if up {
                info!("full-auto: curtain lifted");
            }
            up = false;
        }
        tokio::select! {
            _ = tokio::time::sleep(REAPPLY_EVERY) => {}
            r = run_rx.changed() => {
                if r.is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badge_follows_language() {
        assert!(badge_text("zh-CN").contains("全自动"));
        assert!(badge_text("zh-TW").contains("全自動"));
        assert!(badge_text("ja").contains("フルオート"));
        assert!(badge_text("en").contains("Full auto"));
        assert!(badge_text("").contains("Full auto"));
    }

    #[test]
    fn script_embeds_text_as_a_string_literal() {
        let s = script(true, r#"a"b</script>"#);
        assert!(s.contains(r#"const T="a\"b</script>";"#));
        assert!(s.contains("window.__akagiCurtain=c"));
        let off = script(false, "");
        assert!(off.contains("remove()"));
    }
}
