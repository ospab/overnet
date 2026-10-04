**English** · [Русский](ru/plan.md)

# The plan

*Version 0.1 — 2026-06-23. Fixed decisions and build order. From docs to code.*

> **Since then:** v0.2 (October 2026) replaced the hand-rolled onion with
> Tor-style circuits (ntor + per-hop ChaCha20 cells), added `.ov` services, the
> network service sites and overnet browser. See [running.md](running.md) and
> [decisions.md](decisions.md).

---

## Stack (fixed)

**Rust + tokio**, reusing `ostp`'s crypto crates one-to-one. Zero friction with
phase 1, memory safety for network code, primitives already proven in `ostp`
production.

Crypto (as in `ostp`; we do NOT roll our own): `snow` (Noise), `x25519-dalek`,
`chacha20poly1305` (AEAD), `ed25519-dalek` (identity / signatures).

**License: `AGPL-3.0-only`** — compatible with `ostp` (AGPLv3); copyleft with
the §13 network clause keeps forks and services open (see distribution.md).

## Reusing ostp (decided 2026-06-23)

overnet is **a separate repo** that depends on `ostp-core` by path
(`ostp-core = { path = "../../ostp/ostp-core" }`). Verified to build across
repos. The low and middle layers are **taken from ostp, NOT rewritten**:

- `crypto::reality` — Reality/TLS mimicry (the moat against DPI);
- `protocol::ProtocolMachine` — a reliable Noise+PSK tunnel (sans-io) + padding
  + congestion control + reordering;
- `framing` (`PaddingStrategy`/`AdaptivePadder`) — protection against traffic
  analysis;
- `dnstt`/`dns` — a DNS tunnel as yet another medium.

What ostp does **not** have (and is overnet's new work, added on top): onion
routing, crypto addressing, mesh routing, a transport-agnostic `Link`.
`ostp::relay` is a SOCKS-like proxy exit (point to point), **not** an onion.

**Hop encryption layers:** the internet `Link` = an ostp tunnel (Reality + Noise
built in); a "bare" `Link` (LoRa/Bluetooth, no cipher of its own) = bare Noise
from `overnet-core::session`. overnet's onion layers go on top of any `Link`,
end to end.

## Decisions and trade-offs

| Component | Decision | Trade-off |
|---|---|---|
| Language / runtime | Rust + tokio | — |
| Crypto primitives | snow + x25519-dalek + chacha20poly1305 + ed25519-dalek | we don't invent |
| Identity / address | ed25519 → `addr = sha256(pubkey)` | hex for now, base32 + checksum later |
| Transport | `trait Link` | the core, no compromise |
| Phase 1 transport | the `ostp` tunnel wrapped as a `Link` | reuse Reality, don't rewrite it |
| Privacy | onion, source-routed, 3 hops | MVP: layers + per-hop X25519; strict Sphinx in v2 |
| Mixnet | the hook exists, off in the MVP | forward-compatible, doesn't slow the launch |
| Trust / Sybil | open + vouched, MVP = open only | ed25519 vouchers can be added without a redesign; PoW / anonymous credentials later |
| Naming | crypto address + petnames | no global / blockchain names |
| Bootstrap | invite / QR + built-in nodes | — |
| Repository | open source (AGPL), starting as a local git, `private/` excluded | mirrors / signed releases after the MVP |

**Trade-off philosophy:** a thin end-to-end slice on strong primitives with the
simplest topology; harden incrementally without painting over the path to more
complexity.

## MVP — the "walking skeleton" (v0.0.1)

Three nodes **A → R → B**, each with an ed25519 identity and a crypto address.
A wraps a message in an onion (2 layers) and sends it through relay R. **R sees
only its neighbours — not the content and not the destination.** B peels the
last layer. Underneath is TCP (dev) or an `ostp` tunnel (phase 1), and swapping
the transport changes nothing above it.

Proves at once: the transport-agnostic core + crypto addresses + onion +
integration with `ostp`.

## Workspace layout (flat, as in ostp)

```
overnet/
  Cargo.toml          # workspace
  overnet-core/       # Link, Identity, Address (+ packet, onion)
  overnet-link-tcp/   # Link over TCP
  overnet-link-ostp/  # the ostp tunnel wrapper (phase 1)
  overnet-node/       # node runtime
  overnet-cli/        # dev harness
```

## Build order (milestones)

- [x] **Step 0** — workspace skeleton + `overnet-core`: `Identity` (ed25519),
      `Address` (= sha256(pubkey)), `trait Link`, a loopback `Link` with a test.
- [x] **Milestone 1** — `overnet-link-tcp`: a length-framed TCP `Link`. A frame
      goes A↔B (test).
- [x] **Milestone 2** — a Noise session (`snow`) over `Link` + client↔server echo
      (tests).
- [~] **Milestone 3** — onion: `overnet-core::onion` — building / peeling layers
      (per-hop X25519 + AEAD), the relay doesn't see the payload (3 tests green).
      **Left:** route the onion through nodes over `Link` (A→R→B live) = **MVP**.
- [ ] **Milestone 4** — `overnet-link-ostp`: wrap `ostp::ProtocolMachine` →
      phase 1.
- [ ] **Milestone 5** — neighbour discovery, routing → hardening (Sphinx,
      mixnet, referrals).

## What we DON'T do at the start

Our own crypto primitives; a global namespace / blockchain names; tokens; mixnet
delays; PoW and anonymous credentials; optimizing for scale before the core
works.

## Building

```
cd overnet
cargo check          # check that the workspace builds
cargo test           # run the overnet-core tests
cargo run -p overnet-cli   # generate an identity and show its address
```
