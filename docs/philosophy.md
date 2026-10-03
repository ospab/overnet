# overnet philosophy

*Version 0.1 — 2026-06-23. A living document and a working notebook, not a manifesto set in stone.*

---

## 1. Why

States are moving toward a closed internet: whitelist mode (only what is
explicitly allowed gets through), isolated cross-border links, active probing of
anything unrecognized. Tools like VPNs and proxies live in a permanent arms race
and structurally lose: the endgame of censorship switches off not a particular
protocol but **the channel itself** underneath it.

overnet takes the worst case as a given: **the border is closed, the backbones
are controlled, any unidentified traffic is dropped.** A network designed for
that world will survive milder ones too.

Freedom to exchange information is not a feature here; it is the reason the
project exists.

## 2. The main engineering principle

> The network must survive the hostility of its own infrastructure.

We do not assume that nodes, providers or media are honest. We assume some of
them belong to the adversary. Everything else follows from that: encryption by
default, no central points, routing in which no single node sees the whole
picture.

## 3. Principles

1. **Metadata is the enemy, not just content.**
   Who talks to whom, when and how much is often more dangerous than the message
   itself. Protecting metadata (not only the payload) is a first-class task, not
   an add-on.

2. **Transport is a replaceable part.**
   The logical network is not tied to a medium. An internet tunnel, radio,
   optics, sneakernet — all of them are just edges of the graph. See
   [architecture.md](architecture.md).

3. **An address belongs to a key, not to a registry.**
   A node's address is derived from its public key. There is no central body
   that issues or revokes addresses.

4. **Participant = node.**
   If you use overnet, you relay overnet. This is not only ideology but a
   solution to the relaying problem: everyone carries traffic, not a few
   dedicated sacrificial nodes. It also makes a single participant harder to
   single out in the crowd.

5. **Physical sovereignty is the only complete unblockability.**
   DPI cannot be fooled on a medium you own. That is why phase 3 (our own
   medium) is not romance but the logical limit of the project. The price is
   physical detectability (radio can be direction-found); the design accounts
   for that honestly. See [threat-model.md](threat-model.md).

6. **The honesty boundary: what we encrypt vs what we disguise.**
   In our own network (phases 2–3) we encrypt the whole packet, including the
   control part. Over someone else's internet (phase 1) the outer envelope
   **cannot** be encrypted — other people's routers read it; there we do not
   encrypt the envelope, we **disguise** it as permitted traffic. Confusing the
   two is the classic beginner's mistake.

## 4. Cryptographic engineering ethics

**We do not roll our own crypto.** This is rule #1 for everyone except a handful
of people on the planet, and it is usually broken by exactly those who think
they are the exception.

- We use proven primitives: **X25519** (key exchange), **ChaCha20-Poly1305**
  (authenticated encryption), the **Noise Protocol Framework** for handshakes
  (WireGuard is built on it), **ntor** for circuit handshakes (as in Tor).
- **overnet's novelty is in the construction, not the primitive.** Jason
  Donenfeld became known for WireGuard without inventing a single cipher — he
  put existing ones together so simply and cleanly that the world switched.
  Signal, Tor, Sphinx — same story: respected for the *protocol and system*, not
  for a home-made algorithm.
- **Anything that has not survived public attack has no value.** AES came from
  an open competition. Post-quantum Kyber/Dilithium came from a multi-year NIST
  tournament. djb's authority (Curve25519/ChaCha) comes from his work not being
  broken for 15 years, not from it being new. Our construction must be designed
  so that it *can* one day be put up for public scrutiny.

## 5. What kind of recognition we want (and why it matters for design)

The model is Bellard (FFmpeg) and Bernstein (djb): known to those who understand
what you built, not a second in a feed. This shapes engineering decisions: we
build to **last** and for people to **switch over**, not to impress quickly. A
durable, understandable, documented construction matters more than a flashy
demo.

## 6. What we honestly do NOT promise

- We do not promise resistance to a global passive observer with perfect
  traffic correlation — Tor cannot do that either. See
  [threat-model.md](threat-model.md).
- We do not promise to protect a participant who has been identified physically
  and visited at home.
- We do not promise "we will invent a new unbreakable cipher". We build from
  proven parts.

An honest threat model is part of the philosophy. A network that lies about what
it protects against is more dangerous than no network at all.
