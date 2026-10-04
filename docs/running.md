**English** · [Русский](ru/running.md)

# Running overnet v0.2: circuits, .ov services, the browser

*How to bring up the network, publish a site and open it in the browser; how
overnet works together with ostp.*

---

## What v0.2 is

The network follows Tor's design, without Tor's infrastructure:

- **Circuits.** A client builds a circuit through three relays: CREATE to the
  first, EXTEND onwards. Every hop runs an ntor handshake
  (`overnet-core/src/ntor.rs`): only the holder of the relay's key can answer,
  and the relay learns nothing about the client.
- **Fixed-size cells** (1024 bytes, `cell.rs`) with a ChaCha20 layer per hop.
  Each hop sees only its neighbours; only the recipient sees the content.
- **Streams** inside a circuit (BEGIN/DATA/END) with a SENDME window: TCP, HTTP,
  WebSocket — anything.
- **.ov services** work through introduction points and joined circuits (like
  Tor onion services): neither the client nor the service learns the other's
  address. Introduction points are computed from the service key and the
  directory, so no descriptors need publishing.
- **A service address** is 56 characters + `.ov` (an ed25519 key + checksum)
  and cannot be forged. Short names: the reserved ones are built into the
  client, the rest live at the `name.ov` registrar, and the client checks the
  name owner's signature.
- **Clients never talk to the bootstrap server.** They get the relay directory
  from a relay through a circuit. Bootstrap only knows relays (they are public
  by nature).

The old request/response stack and its commands live under `overnet legacy …`;
the Tauri app still runs on it. The old `REGISTER` weakness (anyone could claim
someone else's key) does not exist in v0.2: a service takes an introduction
point with a signature bound to a specific circuit.

## Install

Release builds are on GitHub Releases (built by `.github/workflows/release.yml`
on `v*` tags). Linux and macOS:

```bash
curl -fsSL https://raw.githubusercontent.com/ospab/overnet/master/scripts/install.sh | sudo bash
```

The binary goes to `/opt/overnet/overnet` (linked from `/usr/local/bin/overnet`),
the config to `/etc/overnet/config.json`, service data to `/var/lib/overnet`.
Running it again updates the binary and restarts overnet services; the config
is left alone. Flags after `bash -s --`:

| Flag | What it does |
|------|--------------|
| `--config-url URL` | download a network config (relays, reserved) on first install |
| `--role relay` | the `overnet-relay` systemd service; directory address from `--advertise IP:4040` or the host IP |
| `--role bootstrap` | relay directory on `0.0.0.0:8080` |
| `--role gateway` | SOCKS5 gateway on `127.0.0.1:9150` |
| `--role site:name` (`search`, `files`, `mail`) | a network service site |
| `--role service:NAME:PORT` | publish `http://127.0.0.1:PORT` as an .ov site; the key is created at `/var/lib/overnet/NAME.key` and the address is printed |
| `-v v0.2.0` | a specific release instead of the latest |
| `--uninstall` | stop the services and remove the binary (config and data stay) |

Windows (PowerShell, no administrator rights needed):

```powershell
irm https://raw.githubusercontent.com/ospab/overnet/master/scripts/install.ps1 | iex
```

This installs overnet to `%LOCALAPPDATA%\Programs\overnet` (added to the user
PATH) and **overnet browser** to `%LOCALAPPDATA%\Programs\overnet-browser`, with
a Start menu shortcut. The config lives at `%LOCALAPPDATA%\overnet\config.json`.
With parameters: `& ([scriptblock]::Create((irm …/install.ps1))) -NoBrowser`
(or `-ConfigUrl https://…/config.json`).

To update later: `overnet update` (on Linux, `sudo overnet update`).

The relays and service addresses of the main network are built into the
program (`SEED_RELAYS` and `PINNED` in `overnet-node/src/net/names.rs`), so a
client needs no config: the browser works right after installation. `relays`
and `reserved` in the config are only for a network of your own — they
override the built-in values.

Without `--config`, overnet looks for its config in: `$OVERNET_CONFIG`,
`./config.json`, `config.json` in the data directory, `/etc/overnet/config.json`.

## Roles and commands

| Who | Command | What it does |
|-----|---------|--------------|
| bootstrap | `overnet bootstrap 0.0.0.0:8080` | relay directory (relays only) |
| relay | `overnet relay --bootstrap B:8080 --advertise IP:4040` | forwards cells; prints its line for `relays` |
| exit | `overnet relay … --exit direct` or `--exit socks5://127.0.0.1:9151` | lets traffic out to the internet; **off by default** |
| site | `overnet service --key shop.key --port 80=127.0.0.1:8080` | any local HTTP server becomes an .ov site |
| service site | `overnet site name\|search\|files\|mail` | registrar, search, files, mail |
| client | `overnet gateway` | SOCKS5 on 127.0.0.1:9150 |
| browser | `overnet browser` | overnet browser (or Mullvad Browser with an overnet profile) |
| update | `overnet update` | install the latest release |

Config (`config.json`, flag `--config`):

```json
{
  "token": "",
  "relays": ["<pubkey hex>@1.2.3.4:4040", "<pubkey hex>@5.6.7.8:4040"],
  "reserved": {
    "name.ov":   "<address>.ov",
    "search.ov": "<address>.ov",
    "mail.ov":   "<address>.ov",
    "files.ov":  "<address>.ov",
    "source.ov": "<address>.ov"
  },
  "gateway": { "listen": "127.0.0.1:9150", "clearnet": "direct" },
  "relay":   { "listen": "0.0.0.0:4040", "bootstrap": "1.2.3.4:8080", "exit": "off" },
  "browser": { "path": "", "gateway": "auto", "clearnet": "block" }
}
```

Give the bootstrap server a `node_ttl_secs` (say 120) so that relays that went
offline drop out of the directory. v0.1 relays (`overnet legacy relay`) do not
understand cells — don't mix them with v0.2 in one directory.

`gateway.clearnet` is where a plain gateway sends everything that isn't `.ov`:
`direct` (as without the gateway; sites see your IP), `exit` (through an
overnet exit) or `block`. In the browser, `browser.clearnet` applies instead,
and it is `block` by default.

## Try it on one machine

```
overnet demo        # 5 relays, name/search/files/mail, a gateway on 127.0.0.1:9150
overnet browser     # in another window
```

The demo sites' keys are kept in `%LOCALAPPDATA%\overnet\demo` (Linux:
`~/.local/share/overnet/demo`), so addresses stay the same between runs.

## Publish your own site

```
overnet keygen shop.key                          # prints the .ov address
overnet service --key shop.key --port 80=127.0.0.1:8080
overnet name-sign shop.ov --key shop.key         # paste at http://name.ov/
```

A site needs no public IP, domain or certificate: the service connects to the
network by itself.

## The browser

**overnet browser** is Mullvad Browser (Firefox ESR with the Tor Project's
anti-fingerprinting, without Tor) repackaged with overnet built in
(`browser/repack.py`, built in GitHub Actions from the latest signed Mullvad
Browser release):

- **Its own gateway.** The browser starts the bundled `overnet` with itself and
  stops it when it closes. There is no terminal window to keep open.
- **overnet in the interface.** A toolbar indicator coloured by network state
  (purple — connected, yellow — connecting, red — the gateway isn't answering);
  clicking it opens a panel with the status, how regular sites are handled,
  **New circuits** and the start page. The same status and **New overnet
  circuits** are at the top of the ☰ menu. On `.ov` sites the address bar shows
  an overnet badge instead of "Not secure": the network encrypts the
  connection end to end.
- **The start page `browser.ov`** is served by the gateway itself, never over
  the network: search, the network services, network status.
- **Regular sites stay outside.** Opening youtube.com or any other non-.ov site
  shows a page explaining that it isn't an overnet site, with **Open in my
  regular browser** and **Go back**. A direct connection would reveal your IP
  next to your .ov visits. With `"browser": {"clearnet": "exit"}` regular sites
  go through overnet exit relays instead, when the network has any.
- **Settings:** SOCKS5 with remote DNS (otherwise `.ov` would leak to the system
  DNS), DoH off, `search.ov` as the search engine, Mullvad's updater and VPN
  extension removed. `.ov` sites open over `http://` — the network does the
  encryption; regular sites still get HTTPS-first.

`overnet browser` starts overnet browser if it is installed. Otherwise it falls
back to Mullvad Browser with a separate overnet profile and its own gateway in
the terminal (keep that window open); failing that, Firefox, with a warning
that it has no anti-fingerprinting. The path can be set in `browser.path` or
`OVERNET_BROWSER`.

**ostp detection** (Mullvad Browser fallback only). Before starting, the
command resolves `name.ov` through the system DNS. If the answer is an ostp
fake address (`198.18.0.0/15`), the ostp VPN is running in TUN mode and its
server brings `.ov` into overnet, so the browser starts without a proxy.
Otherwise a local gateway is started (or a running one reused). Override with
`--gateway auto|always|never`; `always` is useful if you don't want the ostp
server to see which `.ov` sites you open.

**Phones.** overnet browser is desktop-only for now (Windows; Linux and macOS
builds are next). On Android and iOS, overnet works through the ostp app in VPN
mode with a server that has `overnet.entry` enabled: `.ov` then opens in any
browser. `mail.ov` doesn't work there yet, because mobile browsers don't treat
`http://mail.ov` as a secure context. Firefox builds for Android with
`about:config` (IronFox, for example) fix this with the same
`dom.securecontext.allowlist` setting.

## source.ov: the source code inside the network

`source.ov` is the reserved name for a Gitea with overnet's code: the source
stays reachable even when GitHub isn't. Gitea runs as usual on loopback, and
`overnet service` brings it into the network:

```ini
; Gitea app.ini
[server]
DOMAIN       = source.ov
ROOT_URL     = http://source.ov/
HTTP_ADDR    = 127.0.0.1
HTTP_PORT    = 3000
DISABLE_SSH  = true      ; only port 80 goes into the network

[service]
OFFLINE_MODE = true      ; no avatars or CDNs from the regular internet
```

```bash
curl -fsSL …/install.sh | sudo bash -s -- --role service:source:3000
```

The installer creates the key `/var/lib/overnet/source.key` and prints the
address, which goes into clients' `reserved` as `"source.ov"`. The key *is* the
address: keep a copy.

Clone through the gateway: `git -c http.proxy=socks5h://127.0.0.1:9150 clone
http://source.ov/ospab/overnet.git` (`socks5h` so that the gateway resolves the
name).

## Together with ostp

overnet is **never on by default** in ostp: the `overnet` section of an ostp
server is off, and the ostp installer doesn't install overnet. A server owner
turns it on (ostp 0.4.7+, alpha channel) and installs the gateway themselves:
`install.sh --role gateway`. If entry is on and the gateway isn't running,
clients get "connection refused" for `.ov` — a configuration error, not a
silent fallback. In TUN mode `name.ov` gets an address from 198.18.0.0/15 only
when entry is on; `overnet browser`'s detection relies on that.

Next to the ostp server run the overnet gateway and, optionally, an exit relay:

```
overnet gateway                                    # 127.0.0.1:9150 — ostp sends .ov here
overnet relay --exit socks5://127.0.0.1:9151 …     # exit through ostp's rules
```

and in the ostp config:

```json
"overnet": { "enabled": true, "entry": true, "gateway": "127.0.0.1:9150",
             "exit": true, "exit_listen": "127.0.0.1:9151" }
```

`entry` lets ostp clients open `.ov` in any browser with no setup. `exit` lets
overnet traffic out to the internet under that server's rules (outbound,
`bind_ip`, blocklists); the server itself and private networks stay closed.
Details are in ostp's `docs/*/server.md`.

## Honest limits

- **Links between nodes are not encrypted yet.** Cell contents are encrypted in
  layers, but circuit IDs and cell commands are visible on the wire. ostp
  transport (`overnet-link-ostp`) will close this — it's the next step.
- **Anonymity depends on the size of the network.** With five relays it is weak
  against an observer who sees all traffic. In a small network circuits are
  shorter than three hops.
- **No Sybil protection.** Anyone can run many relays. A client keeps one entry
  relay ("guard") for the whole session, but there is no Tor-style guard policy
  yet.
- **An introduction point sees** that someone came to service X, and when — not
  who, and not where the service is.
- **The registrar can refuse** a name or "forget" it, but it cannot swap the
  address: the client checks the owner's signature.
- **Mail:** the server sees which mailbox received a letter, when, and its
  size. The sender inside the letter isn't signed.
