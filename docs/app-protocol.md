**English** · [Русский](ru/app-protocol.md)

# The application layer: protocol, names, client/server, roles

*Version 0.1 — 2026-06-23. "What overnet looks like to a person": what's in the
address bar, how the client and server work, what kinds of nodes there are.*

---

## 1. Self-sufficiency + an optional way out to the clearnet

**By default overnet is self-sufficient.** The destination is always an overnet
node; `.ov` names resolve **inside** the network; a service is reachable over an
onion route **with no public IP and no DNS** (like a Tor onion service). The
clearnet isn't needed at all — that is the "unkillability": there's nothing to
block at the border.

**The optional way out to the clearnet** is separate **exit nodes**, turned on
**explicitly and voluntarily**. To be honest about it: an exit brings back all
the exit-node problems (abuse, blocking, legal claims against the exit). So:
- exits are **off by default**; a node becomes an exit only by a deliberate
  choice;
- leaving overnet is visibly marked (the user sees they're leaving overnet);
- the self-sufficient part **doesn't depend** on whether exits exist.

## 2. Protocol: don't rewrite HTTP — separate the scheme from the wire

HTTP's problem isn't its semantics but its **transport**: cleartext headers,
SNI, metadata leaks, ties to TLS/TCP. In overnet the transport is already solved
(onion + Noise + `Link`), so:

- **The scheme is our own:** `overnet://search.ov`. It's just a marker for
  "speak over overnet" + a name to resolve inside the network. Cheap and visible
  in the address bar.
- **On the wire — HTTP semantics inside an overnet session.** Request/response,
  methods, content types, status codes are universal and reuse the whole web
  (HTML/CSS/JS, rendering, tools). We carry them **inside an encrypted onion
  session**, like HTTP/3 over QUIC or Tor onion services over plain HTTP.

**Why HTTP's flaws disappear this way:** headers / SNI / metadata belong to the
transport layer; inside overnet **there is no cleartext layer for an observer**,
everything is encrypted, control data included. Exactly the "encrypt the whole
packet" goal from the beginning — reached through architecture rather than by
rewriting HTTP.

**A simple start:** HTTP/1.1-style request/response inside a session.
HTTP/2-style multiplexing later. What's "ours" stays tangible: `overnet://`,
`.ov` names, encrypted-by-default, servers needing no public IP or DNS — that's
already a different web.

> **We don't reinvent HTTP semantics from scratch** — that's a tar pit of
> decades of edge cases (caching, ranges, content negotiation, streaming). We do
> our own thing where it buys unblockability (scheme, addressing, encryption),
> not where it's merely pride.

> **v0.2:** `.ov` sites are opened as `http://name.ov` (like `.onion` in Tor
> Browser); the `overnet://` scheme doesn't work in Firefox's address bar without
> engine patches.

## 3. Names and bootstrap

- **`name.ov`** — a human-readable name (the petname layer) that resolves to the
  service's **crypto address** (= f(public key), see naming.md). No domains, no
  central DNS.
- **`search.ov`** — a well-known starting service (search + a directory of
  names). Its crypto address (public key) is **built into the client** so there
  is somewhere to start — like Tor's / Yggdrasil's bootstrap and directory nodes.
  It's a "known" node, **not** a "privileged" one: there can be several built-in
  services, and they can change.
- Resolving `name.ov → crypto address` is done by **directory nodes** (see
  roles) with signed records; answers are cached and checked against the
  signature.

## 4. The client

An overnet client is **a network node** (it relays by default: "participant =
node") **plus a local gateway** through which a person looks at overnet.

Realistically, step by step:
- **First:** a local gateway / proxy. The client keeps a local endpoint that
  `overnet://` requests go through; a simple UI renders the responses. An OS
  handler for the `overnet://` scheme, so links open the client. (As I2P
  proxies / Tor do.)
- **Decision (v0.2):** we don't write our own browser engine. **overnet
  browser** is the ready-made Mullvad Browser (Firefox ESR with the Tor
  Project's anti-fingerprinting) repackaged with overnet built in: its own
  gateway, a network indicator, and regular sites kept outside. Addresses are
  `http://name.ov`. See [running.md](running.md).

Bootstrap on first launch: generate an identity (ed25519 → crypto address),
find the first neighbour (an invite / QR from a friend, built-in bootstrap
nodes, local discovery), solve a PoW (later). See naming.md, "First contact".

## 5. The server / service

An overnet server is a node that **answers at its crypto address** (an onion
service). Properties:
- **No public IP or DNS needed** — reachable over onion + rendezvous even behind
  NAT.
- **Address = key** — `search.ov` is a petname over the server's crypto
  address.
- Serves content with HTTP semantics (see §2): `GET overnet://search.ov/` → a
  page.

That removes a huge barrier: "putting up a site" = running a node with a key,
with no hosting, domain, public IP or certificates.

## 6. Node roles are capabilities, NOT a status hierarchy

An honest correction: **a privileged "status hierarchy" is dangerous** — it
recreates centralization and a list of targets (like Tor's public relay list).
In overnet a node **optionally takes on roles** (capabilities), while trust is a
separate, orthogonal matter of **open / vouched tiers** (see architecture §2).

Roles (a node can combine several):

| Role | What it does | Default |
|------|--------------|---------|
| **Relay** | relays its neighbours' onion traffic | **on** (participant = node) |
| **Service** | hosts an `.ov` service (answers at its crypto address) | optional |
| **Directory** | serves signed `name.ov → crypto address` records, helps search | optional |
| **Rendezvous** | brings together two nodes behind NAT (an onion service's meeting point) | optional |
| **Guard** | someone's personal entry (hidden, not a public list) | the client's choice |
| **Exit** | lets traffic out to the clearnet | **off**, only deliberately |

There's no "hierarchy" in the sense of rank. There are:
- **capabilities** — what a node can do / has agreed to do;
- **trust tiers** — open (anyone) vs vouched (by referral), orthogonal to roles;
- **well-known services** (`search.ov`) — "known / built in", not "higher up".

## 7. "A network over a network" — what a test looks like

- Bring up several nodes (e.g. 3 abroad + 1 at home). Each is a relay.
- One of them is a **service** (`search.ov`-like) with a known key.
- A client at home: `overnet://<service>/` → the request goes along an onion
  route through the relays, the service answers with content. Relays see only
  their neighbours, not the content.
- First the transport `Link` = TCP/ostp over the internet (phase 1); the same
  application layer later rides on physical `Link`s unchanged.

## 8. Open questions

- The exact request/response format: take HTTP/1.1 as is, or trim the headers a
  little.
- Resolving `name.ov`: the trust model for directory records (multi-signature?
  in-person fingerprint checks, like a safety number?).
- ~~Where the UI lives: a separate gateway app vs a browser fork, and when.~~
  → overnet browser, a repackaged Mullvad Browser (2026-10-04).
- ~~Marking the way out to the clearnet in the UI.~~ → overnet browser shows a
  "not an overnet site" page before any regular site; with exits enabled it
  goes through overnet exit relays.
