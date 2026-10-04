**English** · [Русский](README.ru.md)

# overnet

> A network that survives the hostility of its own infrastructure.

**overnet** is a censorship-resistant network: transport-agnostic,
cryptographically addressed, with decentralized routing. The goal is
communication that cannot be switched off — not by DPI blocking, not by closing
cross-border links, not by pressure on any single operator.

This is not "yet another VPN". It is an attempt to build a **logical network
that does not care about its physical medium**: the same address and the same
route work over an internet tunnel today and over radio or optics tomorrow.

---

## The core thesis

Censorship resistance is not a single trick but **three layers over time**:

| Phase | What it is | Status |
|-------|------------|--------|
| **1. Overlay** | On top of the existing internet, disguised as permitted traffic (like `ostp`). Useful to people today. | groundwork exists (`ostp`) |
| **2. P2P mesh** | A self-sufficient darknet: onion routing; exits to the clearnet are optional and off by default. | v0.2 (see below) |
| **3. Physical** | Our own physical medium (directional antennas, LoRa, optics) where density allows. The one thing DPI cannot block in principle. | long term |

Picking only one of the three is a losing move. They are built one after
another, **on a single core**.

## The one architectural decision that defines everything

**A transport-agnostic core.** The network layer does not know *what* the link
between nodes is — it is just an edge in a graph. An edge can be a TCP tunnel
through the censored internet, a directional Wi-Fi antenna, LoRa radio,
Bluetooth between phones in a crowd, or even a USB stick (store-and-forward).
When the state chokes internet transport, **the same addresses, the same
routing and the same applications** move to a physical transport. That is
survivability.

A node's address is derived from its public key. No RIR, no BGP, no central
point that can be taken away.

## Documents

- [docs/philosophy.md](docs/philosophy.md) — why, principles, engineering ethics.
- [docs/architecture.md](docs/architecture.md) — layers, phases, the transport-agnostic core, trust topology.
- [docs/threat-model.md](docs/threat-model.md) — who we defend against, what they can do, and what we honestly do **not** protect.
- [docs/naming.md](docs/naming.md) — how participants address each other (crypto addresses + petnames, no domains).
- [docs/distribution.md](docs/distribution.md) — open source, mirrors, getting the app to people.
- [docs/roadmap.md](docs/roadmap.md) — phases and concrete first development steps.
- [docs/glossary.md](docs/glossary.md) — **a plain-language cheat sheet** (onion, mixnet, guard, PoW, Sybil…).
- [docs/app-protocol.md](docs/app-protocol.md) — the application layer: `.ov` names, client/server, node roles.
- [docs/plan.md](docs/plan.md) — the overall plan, stack, reuse of `ostp`, milestones.
- [docs/running.md](docs/running.md) — **how to run v0.2**: relays, services, the browser, ostp.

> The author's personal notes live in `private/` and are **excluded from the
> repository** (`.gitignore`). They never reach the public repo.

## Status

**v0.2:** Tor-style circuits and streams, `.ov` services through intro points,
a SOCKS5 gateway, network service sites (`name.ov`, `search.ov`, `files.ov`,
`mail.ov`, `source.ov`) and **overnet browser** — Mullvad Browser repackaged
with overnet built in (its own gateway, a network indicator, regular sites kept
outside). Together with
ostp: an ostp server brings its clients into `.ov` and can act as an overnet
exit. How to run it — [docs/running.md](docs/running.md), which also has an
honest list of what is still missing.

## Install

Linux and macOS:

```bash
curl -fsSL https://raw.githubusercontent.com/ospab/overnet/master/scripts/install.sh | sudo bash
```

Windows (PowerShell, no administrator rights needed) — overnet and overnet
browser:

```powershell
irm https://raw.githubusercontent.com/ospab/overnet/master/scripts/install.ps1 | iex
```

Update later with `overnet update`.

A relay or a network site on a Linux server, as a systemd service:
`… | sudo bash -s -- --role relay` (see `install.sh --help` and
[docs/running.md](docs/running.md#install)). The source code is also served
inside the network at `http://source.ov/`.

## The principle that must not be broken

We **do not invent cryptographic primitives**. We take proven ones (X25519,
ChaCha20-Poly1305, Noise, ntor) and build a **construction** out of them.
overnet's novelty is in the protocol and the architecture for a specific
threat, not in a home-made cipher. See [philosophy.md](docs/philosophy.md).

## License

AGPL-3.0-only — see [LICENSE](LICENSE).
