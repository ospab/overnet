**English** · [Русский](ru/decisions.md)

# Decision log

*Short records of changes in direction, so the project doesn't drift. Newest first.*

---

## 2026-10-04 — overnet browser: a repackaged Mullvad Browser

**Decided:** the browser is **overnet browser** — the official Mullvad Browser
build repackaged in GitHub Actions (`browser/repack.py`): our branding, our
settings and policies, an autoconfig script that starts the bundled overnet
gateway and builds overnet into the interface. Gecko itself is not rebuilt, so
the Tor Project's anti-fingerprinting stays intact and updates are cheap: a new
Mullvad release means a new repack.

**Supersedes** item 4 of 2026-06-23 (Chromium/Electron) and the 2026-10-01
choice of "Mullvad Browser with a separate profile": a profile could not remove
Mullvad's branding or put overnet into the UI.

Regular (non-.ov) sites are blocked in the browser by default: a direct
connection would reveal the user's IP next to their .ov visits.

## 2026-10-01 — Browser: Gecko only

Chromium (and with it Electron) is out: the browser must be Firefox's engine
with Tor-grade anti-fingerprinting. The light path: Mullvad Browser on desktop,
a GeckoView app on Android later. A full tor-browser-build fork is too heavy for
now.

## 2026-06-23 — Simplification: transport, censorship circumvention, browser

**Context:** limited resources (no servers in Russia, no wish to wrestle with
CDNs) and real experience: Reality doesn't get through whitelists, plain TCP
holds up better on mobile networks.

**Decided:**
1. **Reality / TLS mimicry / xhttp are out of the main path.** The carrier
   `Link` is a plain `TcpLink`. (`OstpLink`/`overnet-link-ostp` stays in the
   repo as an option for the future — the transport-agnostic core lets it come
   back with one line — but it isn't used now.)
2. **Censorship circumvention is NOT overnet's job.** A separate VPN does that;
   overnet rides on top of any IP channel (home Wi-Fi or a commercial VPN). The
   honest price: if the channel goes down, overnet-over-the-internet goes down
   too; the answer to that is the physical mesh (phase 3), which is long-term.
3. **For now — a network over Wi-Fi.** Mobile devices later.
4. ~~**The browser is Chromium-based (Electron), not Tauri and not a fork.**~~
   *(superseded on 2026-10-01 and 2026-10-04)* Decoupled through a **local HTTP
   gateway**: the overnet core opens `127.0.0.1:PORT`
   (`overnet_node::web::run_gateway`) and the browser maps `overnet://` to it.
   The choice of browser is not baked into the core.

**Effect on the phases:** phase 1 is now "overnet over the regular internet / a
VPN", without its own obfuscation. The philosophy of self-sufficiency and
unblockability remains the goal of the physical mesh (phase 3) — postponed, not
cancelled.
