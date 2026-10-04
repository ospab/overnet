**English** · [Русский](ru/architecture.md)

# overnet architecture

*Version 0.1 — 2026-06-23. A draft. Much is still open — open questions are marked explicitly.*

---

## 0. The central decision

Everything rests on one thing:

> **A transport-agnostic core + cryptographic addressing + decentralized
> routing.**

If that is done right, everything else (phases, media, applications) becomes a
replaceable part. If it is done wrong, the project is dead no matter how good
the cipher is.

## 1. Layers

overnet deliberately **does not throw away** the idea of layers (OSI / packet
switching is good engineering). It replaces only the points that make the
classic stack blockable: centralized addressing (RIRs), centralized routing
(BGP) and dependence on a licensed medium.

```
┌──────────────────────────────────────────────┐
│  Applications (messenger, files, publishing) │
├──────────────────────────────────────────────┤
│  Sessions / streams (reliable delivery)      │
├──────────────────────────────────────────────┤
│  Onion routing                               │  ← the core
│  Address = f(public key)                     │
├──────────────────────────────────────────────┤
│  Transport abstraction (Link = graph edge)   │  ← the key to survivability
├───────────┬──────────┬──────────┬────────────┤
│ TCP /     │ Wi-Fi    │ LoRa /   │ Bluetooth /│  ← replaceable media
│ ostp      │ antenna  │ radio    │ sneakernet │
└───────────┴──────────┴──────────┴────────────┘
```

### 1.1. Transport abstraction (Link)

The most important and most testable part. A `Link` is a bidirectional
frame-delivery channel between two neighbouring nodes. The core does not care
what is underneath:

- **TCP / ostp tunnel** — an internet edge (phase 1, reusing `ostp`).
- **Point-to-point Wi-Fi** (directional antenna) — a physical edge (phase 3).
- **LoRa / packet radio** — a long-range, narrow-band edge (text, store-and-forward).
- **Bluetooth / Wi-Fi Direct** — local edges between phones without internet.
- **Sneakernet** — an asynchronous edge (USB stick / QR), store-and-forward.

The `Link` contract (draft):
```
trait Link {
    fn send(frame: &[u8]) -> Result<()>;       // send a frame to the neighbour
    fn recv() -> Result<Vec<u8>>;               // receive a frame from the neighbour
    fn mtu() -> usize;                          // max frame size
    fn properties() -> LinkProps;               // latency, bandwidth, reliability, directionality
}
```
`properties()` is needed by routing to choose a path (you cannot push video over
LoRa).

**Open question:** one frame format for all media, or adapters for each.
Probably adapters with a common inner packet.

### 1.2. Addressing

A node's address is derived from its long-term public key (e.g. a hash of an
Ed25519/X25519 key). Properties:

- **Self-assigned** — a node generates a key and has an address. No registry.
- **Self-authenticating** — knowing the address, you know whose key is behind
  it; a node cannot be impersonated without its private key.
- **Not centrally revocable** — there is no authority that can "ban" an address.

**Open question:** how to do human-readable names (petnames / web-of-trust
naming) without introducing a centralized DNS. v0.2 answer: self-authenticating
`.ov` addresses plus a registrar whose records the client verifies — see
[naming.md](naming.md) and [running.md](running.md).

### 1.3. Onion routing (the privacy core)

Each hop knows only its predecessor and successor — never the whole picture.
This is **onion routing**, and it has to be a strict, battle-tested version
(**Sphinx**), not the naive "try to decrypt the header → failed → flood" (that
leads to flooding and does not scale, and RSA per hop kills speed).

Sphinx mechanics in short: the source puts an ephemeral public key in the
header; the right hop derives its layer key deterministically with **one X25519
ECDH**, decrypts its layer, reads an explicit "next hop = X", strips the layer
and sends it on to a single recipient. Fixed-length packets (route length does
not leak), replay protection. A detailed packet and crypto model belongs in a
separate document, `packet-crypto.md`.

> **v0.2 status:** circuits are built Tor-style instead — an ntor handshake per
> hop, fixed-size 1024-byte cells, one ChaCha20 layer per hop, streams with
> SENDME windows. Sphinx remains the target for asynchronous / store-and-forward
> traffic. See [running.md](running.md).

**Exits are optional.** By default the destination is an overnet node, not a
clearnet site, so the classic exit-node problems (abuse, blocking, legal claims
against the exit) do not arise. Exits to the clearnet exist only as an opt-in
node role, off by default and clearly marked — the self-sufficient part of the
network does not depend on them.

## 2. Trust topology and guard selection

Open membership is vulnerable to a Sybil attack: the state floods the network
with thousands of its own nodes to deanonymize users and break routes. Creating
an identity costs us **nothing** (address = f(key)), so "a hundred hostile
nodes" is the default, not a hypothesis.

**A trust anchor comes in only three kinds — there is no fourth door:**
a central authority (rejected — it can be seized or coerced), a scarce resource
(PoW / stake), or a social graph (F2F / web-of-trust). Behavioural heuristics
("the node behaves stably, so we trust it") are **NOT** a fourth door but a
gameable substitute: a patient state makes its nodes more stable and older than
real people's, and the heuristic will end up choosing **exactly the adversary**.

### Decision (taken 2026-06-23)

**Baseline (for all participants):**
- **a mixnet flavour of onion** (delays + cover traffic + padding) against timing
  correlation (see glossary, threat-model §4);
- **personal, hidden guards** — each participant picks THEIR OWN entry points
  and does not disclose them. A network-wide public guard list is a target list
  (a mistake Tor's open relay list suffers from — hence "bridges");
- **path diversity** — never take two hops from the same subnet / AS / operator;
- **expensive identities (PoW)** — raise the price of Sybil from "free" to
  "costly".

**Local behaviour monitoring = protection only, never assignment.** A client
may locally and privately **drop** a guard that behaves suspiciously (breaks
circuits, probes). It has **no** right to assign anyone a network-wide trust
status and broadcast it: self-attestation is useless (the villains are exactly
the ones who lie), and voting can be gamed by Sybils.

**An optional hard tier — F2F / web-of-trust** for high-risk participants:
direct links only with people you know. Benefits: resistance to infiltration,
a perfect fit for a physical mesh, a natural match with "participant = node".

Result: anonymity **degrades gracefully** as the adversary's share grows,
instead of dropping to zero at the first hundred hostile nodes. We do not
promise full resistance to a global correlator (see threat-model §4).

**Open question (a big one):** the boundary between open growth (baseline) and
the F2F core (hard tier). The hybrid is not formalized yet.

## 3. Deployment phases

One codebase, three medium phases:

**Phase 1 — Overlay (today).**
Over the internet, disguised as permitted traffic. The internet `Link` wraps
`ostp_core::protocol::ProtocolMachine` (Noise + padding + header obfuscation;
Reality was removed from ostp in 0.4.0) — reused, not rewritten (overnet depends
on `ostp-core` by path). Gives value and users right now and exercises the core.
overnet's onion layers sit on top.

**Phase 2 — P2P mesh (a self-sufficient darknet).**
Onion routing, F2F topology, NAT traversal / hole punching for direct p2p over
the internet. The network becomes logically self-sufficient — overnet clients
talk to overnet clients without depending on a way out to the clearnet.

**Phase 3 — Physical (the endgame).**
Our own medium where density allows: directional Wi-Fi antennas, LoRa, optics.
The same stack over physical `Link`s. The one thing DPI cannot block in
principle, at the price of physical detectability (see threat-model).

## 4. The "encrypt vs disguise" boundary

A hard consequence to keep in mind at all times:

- **In our own network (phases 2–3)** all forwarding nodes are ours, so the
  whole packet is encrypted; only local addressing for the neighbouring physical
  hop stays in the clear (it is meaningless to a global observer).
- **Over someone else's internet (phase 1)** the outer IP header **cannot** be
  encrypted — other people's routers route it. There we do not hide the
  envelope, we **disguise** it as permitted traffic (ostp). Everything valuable
  is inside.

And separately: **encryption alone does not beat DPI.** High-entropy "garbage
without a TLS handshake" is a glaring anomaly that a whitelist regime will drop
by its shape without reading it. So we need either mimicry (phase 1) or our own
medium (phase 3), plus protection against traffic analysis (padding to a fixed
length, mixing — mixnet territory: Loopix / Nym).

## 5. Open problems (honestly)

The protocol is the easy part. These are the killers:

1. **Routing scalability.** A flat mesh cannot carry millions of nodes (cjdns
   showed that). A hierarchy / DHT is needed — not solved.
2. **Sybil / trust.** See §2. F2F helps but breaks open growth.
3. **Incentives / participation.** "Participant = node" relies on ideology. What
   happens when enthusiasm fades is open. Tokens are not considered for now (a
   regulatory and moral trap).
4. **Physical detectability (phase 3).** Radio can be direction-found. Network
   unblockability is bought with the operator's physical risk.
5. **NAT traversal (phases 1–2).** Carrier-grade NAT gets in the way of direct
   p2p; hole punching and relays are needed.

## 6. Whose shoulders we stand on (study before writing our own)

- **Reticulum (RNS)** — a transport-agnostic crypto stack over any medium;
  closest to our vision, study first.
- **Yggdrasil** — crypto addresses from keys + self-routing.
- **Sphinx** (Danezis & Goldberg) — the onion packet format.
- **Tor ntor**, **Noise Framework**, **WireGuard** — how to do handshakes without
  hand-rolling crypto.
- **Briar** — F2F + multi-transport (Tor / Bluetooth / Wi-Fi Direct), offline.
- **Loopix / Nym** — protecting metadata from traffic analysis.

Perhaps our contribution is not a new stack but a privacy / incentives / UX
layer on top of an existing one. We decide that after living with Reticulum and
Yggdrasil for a week.
