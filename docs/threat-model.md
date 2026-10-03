# overnet threat model

*Version 0.1 — 2026-06-23. Without this document the rest of the design is guesswork.*

---

A threat model answers three questions: **who** we defend against, **what they
can do**, and what we **honestly do not protect**. Every architectural decision
must refer back here.

## 1. Adversaries

### A. A state network censor (the main one) — national DPI
A DPI operator at the country / provider level. Goal: keep communication from
leaving the controlled perimeter and/or suppress unwanted exchange inside it.

### B. A local provider under coercion
Sees a specific subscriber's traffic, can shape or cut it, is obliged to
cooperate with A and keep logs.

### C. Malicious nodes inside the network (Sybil / infiltration)
The adversary runs its own overnet nodes to deanonymize participants, map the
topology, and break or intercept routes.

### D. A physical pursuer (phase 3 only)
Can direction-find radio emissions, physically locate and seize a transmitter or
node, and visit the operator.

## 2. What adversary A (DPI) can do — in increasing severity

1. **Blocking by IP / subnet** — address blacklists.
2. **Blocking by SNI** — reads the site name in the cleartext TLS ClientHello.
   Currently the main tool. (The industry's answer is ECH, encrypted SNI.)
3. **Protocol fingerprinting** — JA3/JA4 from the TLS handshake, signatures of
   known VPNs / proxies (OpenVPN, WireGuard, obfuscators).
4. **Active probing** — on seeing a suspicious connection, it connects to the
   same address / port itself to check whether it is a proxy.
5. **Traffic analysis** — packet sizes, timings, volumes, fan-in / fan-out,
   **entropy**. A high-entropy stream without a recognizable handshake is
   flagged as "unknown crypto" on its own.
6. **Whitelist mode (the endgame)** — only traffic positively identified as
   permitted passes; everything else is dropped by default. **This kills any
   overlay that merely "looks random".** So phase 1 has to *mimic what is
   permitted*, not just be encrypted.
7. **Controlling cross-border links / closing the border** — isolating the
   national internet. Kills any way out of the perimeter at the channel level.
   **The main argument for phases 2–3.**

## 3. What overnet protects (security goals)

| Property | Phase 1 | Phase 2 | Phase 3 |
|----------|:-------:|:-------:|:-------:|
| Content confidentiality | ✅ | ✅ | ✅ |
| Metadata privacy (who talks to whom) | partial | ✅ (onion) | ✅ (onion) |
| Availability under IP/SNI blocking | ✅ (mimicry) | ✅ | ✅ |
| Survival under whitelist mode | ⚠️ depends on mimicry quality | ⚠️ if at least one cross-border link exists | ✅ |
| Survival with the border closed | ❌ | ❌ (without own links) | ✅ |
| No single point of failure | ⚠️ | ✅ | ✅ |
| Sybil resistance | — | ⚠️ (F2F helps) | ✅ (physical proximity) |

How to read it: the further along the phases, the less we depend on the
adversary's infrastructure.

## 4. What we honestly do NOT protect (the limits)

A network that lies about its guarantees is more dangerous than no network.

- **A global passive observer with perfect traffic correlation.**
  If the adversary sees entry and exit *at the same time* and can correlate
  timings across the whole network, onion routing does not save you. Tor does
  not solve this either. Partial protection is mixnets (delays + cover traffic,
  Loopix / Nym), but that is expensive and outside the first iteration.
- **A compromised end device.** If the participant's phone or PC is infected,
  crypto on the wire is meaningless. This is outside overnet's scope.
- **Physical capture of a node (phase 3).** A found transmitter can be seized
  and examined. The design must limit what a node *knows* (F2F: only its
  neighbours) so that seizing one does not expose the network.
- **Protecting a personally targeted participant.** If the adversary goes after
  a *specific* person (physical surveillance, seizure, coercion), that is outside
  the model; overnet protects communication, not the operator's personal safety
  against targeted measures.
- **Anonymity of a radio transmitter (phase 3).** Radio can be direction-found.
  Physical unblockability is bought with the transmitter's detectability. This
  is stated honestly: power, directionality and deniability are design
  parameters, not a promise of invisibility.

## 5. Consequences for the design

Each one follows directly from the points above:

1. **Encryption by default; metadata as a first-class goal** (against A, C).
2. **Phase 1 disguises rather than "encrypting the envelope"** — otherwise a
   whitelist drops it by its shape (item 2.6).
3. **Phases 2–3 do not depend on a way out to the clearnet** — otherwise closing
   the border (item 2.7) kills the network.
4. **F2F topology** — against Sybil / infiltration (C) and to limit what a node
   knows when seized (D).
5. **A node knows only its neighbours** (onion + F2F) — so compromising one does
   not collapse everyone else's anonymity.
6. **No central points** (address from key, no DNS / RIR / BGP) — so there is no
   lever the adversary can seize or coerce.

## 6. Open

- The exact boundary between the F2F core and opennet edges (trust vs growth).
- Whether mixnet delays belong in the baseline or as an option for especially
  sensitive traffic.
- A threat model for naming (petnames) — a separate sub-task.
