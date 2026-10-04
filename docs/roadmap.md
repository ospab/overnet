**English** · [Русский](ru/roadmap.md)

# Roadmap

*Version 0.1 — 2026-06-23. From "read the docs" to "writing code". The order is
deliberate: prove the core first, then build up.*

> **Status, v0.2 (October 2026):** the transport abstraction, keys and
> addresses, Tor-style onion circuits, `.ov` services, the network service sites
> and overnet browser exist — see [running.md](running.md). The milestones below
> are kept as the original plan; Sphinx, mixnet delays and phases 2–3 are still
> ahead.

---

## Ordering principle

We don't build everything at once. First comes **the riskiest architectural
assumption** (the transport-agnostic core): if it doesn't come together cleanly,
the project makes no sense, and it's better to learn that from a small prototype
than a year later.

## Phase 0 — foundations (now, before code)

- [x] Philosophy, architecture, threat model, naming, distribution, glossary.
- [ ] **`docs/packet-crypto.md`** — the packet and crypto model step by step
      (our sketch vs Sphinx, choice of primitives, the "encrypt vs disguise"
      boundary). Also an entry point into cryptography.
- [ ] **Live with prior-art prototypes for a week** before writing our own:
  - **Reticulum (RNS)** — closest to our vision (a transport-agnostic mesh stack);
  - **Yggdrasil** — crypto addresses + self-routing;
  - **Briar** — F2F + multi-transport + offline UX;
  - read the **Sphinx** paper and the **Noise Framework** docs.
  Outcome of this step: write our own core, or build a layer on top of an
  existing one (Reticulum?).

## Milestone 1 — the transport abstraction (first code)

Goal: **prove that the same frame gets from A to B regardless of the medium.**

- [ ] Define the `Link` trait (send/recv/mtu/properties).
- [ ] Adapter 1: **in-process loopback** (for tests, no network).
- [ ] Adapter 2: **TCP**.
- [ ] Test: a frame passes through both adapters the same way.

If that works cleanly, overnet has its foundation.

## Milestone 2 — identity and a secure channel

- [ ] A long-term key = identity + crypto address (see naming.md).
- [ ] A handshake between two nodes using **an existing Noise library** (NOT our
      own code).
- [ ] An encrypted session on top of `Link`.

## Milestone 3 — a minimal onion

- [ ] A source-routed onion over 3 nodes (Sphinx-lite, a proof of concept, not
      production).
- [ ] Check: the middle hop sees only its neighbours, not the content or the
      destination.

## Milestone 4 — mesh routing

- [ ] Neighbour discovery, simple multi-hop routing.
- [ ] (Optional) mixnet delays + cover traffic as a parameter.

## Milestone 5 — integration with phase 1

- [ ] `ostp` (Reality/TLS mimicry) as the internet `Link`. The network goes out
      to people over the existing internet and the core gets tested by real
      users.

## Beyond — phases 2 and 3

A self-sufficient p2p mesh (NAT traversal, the F2F tier), then physical `Link`s
(Wi-Fi antennas, LoRa). As density and readiness allow.

---

## Stack (a proposal, not dogma)

- **Rust** for the core: memory safety is critical for network and crypto code,
  great cross-platform support, bindings for mobile. Experience from `ostp` can
  be reused.
- Crypto — **existing libraries** (Noise, dalek/X25519, RustCrypto AEAD). Not
  our own.
- The final choice of language is recorded as a separate decision.

## What we explicitly DON'T do at the start

- Our own crypto primitives.
- A global namespace / blockchain names.
- Tokens / incentives.
- Optimizing for scale before the core works at all.
