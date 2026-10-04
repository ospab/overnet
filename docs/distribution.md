**English** · [Русский](ru/distribution.md)

# Distribution: the code and the app

*Version 0.1 — 2026-06-23. Two different questions that often get mixed up:
where the SOURCE lives, and how the app reaches PEOPLE.*

---

## Part 1. The source code is open

**Decision: the code and the protocol are open source. For a project like this
it is not up for debate.**

Why:

- **Kerckhoffs's principle** — a system must stay secure even when the adversary
  knows everything about it. Security lives in the keys, not in a secret design.
  "Security through obscurity" is a known failure.
- **Trust = auditability.** The target audience (activists, journalists,
  technically literate people) won't run an anti-censorship tool that can't be
  checked. Closed code in this field is a red flag. Tor, Signal, WireGuard,
  Briar — all open.
- **The kind of recognition we want.** A Bellard/djb-level reputation is only
  built in the open. Closed code doesn't get you onto "that list".
- **Survivability.** Open code can't be killed by removing one person: forks
  survive.

### Where to host

- **Mirrors, not a single platform.** GitHub has already been throttled and
  blocked in Russia, so it can't be the only one. Keep mirrors: **Codeberg**
  (non-profit, Forgejo-based), **GitLab**, optionally a self-hosted Forgejo —
  and inside overnet itself, `source.ov`. Push to all of them at once.
- **Signed releases** (GPG/minisign) — so users can check that a binary comes
  from us.
- **Reproducible builds** — so anyone can build from source and confirm the
  binary matches and has no backdoor baked in. A goal we're working towards;
  for an anti-censorship project it's close to mandatory.

### The author's identity — the author's decision

A public repo ties the project to the author's GitHub account and name. For a
project aimed at state DPI that is a **personal risk**. Pseudonymous maintenance
is a legitimate and common choice (Bitcoin started as Satoshi; many privacy
projects are pseudonymous). Opening the **code** and making **your name** public
are different things; you can do the first without the second. It is a
conscious choice about risk tolerance, made before the first push.

## Part 2. Getting the app to people is a separate task

An open repository ≠ a delivery channel. Different logic, different pain.

### The pre-emptive distribution problem

An anti-blocking tool has to be handed out **before** the blackout. Once the
border is closed, app stores and GitHub are unreachable — there's nowhere left
to download from. So distribution has to happen **while the internet is open**,
with offline channels planned in.

### Delivery channels

- **F-Droid / our own APK repository** — for Android, without Google.
  Sideloading.
- **A direct APK from mirrors + a signature** — in case the stores are blocked.
- **P2P transfer of the installer** — the app can **share itself** with a
  neighbour over Bluetooth / Wi-Fi Direct (like Briar, Bridgefy). That's
  distribution that survives a blackout: one person with the app "infects"
  their neighbours offline.
- **iOS** — the narrowest spot (Apple forbids sideloading in most regions);
  maybe later, with difficulties. Not a launch priority.

### Growth = invitations (and that is also the trust anchor)

Word of mouth through invites is both the growth model and Sybil protection
(see naming.md, architecture §2). One mechanism solves both: an invited person
arrives already "introduced" by a trusted participant.

## Loudness vs safety

Open code helps. Loud advertising of an "unkillable network" paints a target on
the author and early participants, and in phase 3 (radio) adds physical risk.
These are in direct conflict and must be kept in mind. The realistic path is not
virality but **reputation among those who already feel the pain**.

## Open questions

- ~~License~~ → **`AGPL-3.0-only`** (decided 2026-06-23): compatible with `ostp`
  (AGPLv3); copyleft with the §13 network clause keeps forks **and services**
  open — consistent with the philosophy.
- Whether to run our own Forgejo right away or start with Codeberg + a GitHub
  mirror.
- An update strategy for when the main channel is blocked (updates over the
  overnet network itself?).
