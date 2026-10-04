**English** · [Русский](ru/naming.md)

# Naming and addressing

*Version 0.1 — 2026-06-23. How overnet participants find and address each other.*

---

## Theory: Zooko's triangle

A name in a network can have three properties. The classic result: **you cannot
have all three at once — pick two:**

- **human-readable** (easy to remember and dictate),
- **secure** (globally unique, unforgeable),
- **decentralized** (no central authority that issues / revokes names).

| Approach | Readable | Secure | Decentralized | Example |
|----------|:--------:|:------:|:-------------:|---------|
| DNS / domains | ✅ | ✅ | ❌ | the regular internet |
| Public key hash | ❌ | ✅ | ✅ | Tor `.onion` |
| Petnames (local nicknames) | ✅ | ❌ | ✅ | Briar, Scuttlebutt |

**overnet has no domains.** A domain requires a registrar and a root — a centre
that can be seized or coerced. We give up a central point deliberately (see
philosophy §3), so domains are excluded by design.

## overnet's scheme: two layers

### Layer 1 — the crypto address (the "true name")

A node's address is derived from its long-term public key (a hash or the
Ed25519/X25519 key itself + a checksum + a version, base32-like encoding). It
looks ugly — a long string of random characters, like an `.onion` address. But it
is:

- **unforgeable** — you cannot pose as someone else's address without their
  private key;
- **self-assigned** — generate a key and you have an address, no registry;
- **decentralized** — nobody issues or revokes it.

This is the machine layer: the protocol works with it, not people.

### Layer 2 — petnames + web-of-trust (the "human name")

On top of crypto addresses there are **local nicknames** that everyone assigns
themselves: you save a friend's long address as "Pete". There is no global
uniqueness (your "Pete" ≠ my "Pete"), and that is fine: names are local, like
contacts in a phone.

Names spread through **introductions**: someone you trust "introduces" a new
participant to you along with their crypto address. That is how Briar and
Scuttlebutt work. It fits the F2F tier and invitation-based growth for free.

## v0.2 in practice

- **Service addresses** are `<56 characters>.ov`: base32 of the ed25519 public
  key + a 2-byte checksum + a version byte, like Tor v3 `.onion`. A typo fails
  the checksum. See `overnet-core/src/ovaddr.rs`.
- **Reserved names** — `name.ov` (registrar), `search.ov`, `mail.ov`,
  `files.ov` — are pinned in the client (or set in its config), the same way
  Tor ships its directory authorities.
- **Other short names** (`shop.ov`) are looked up at the `name.ov` registrar
  through overnet. Every record carries the owner's signature over the name,
  made with the service key; the client checks it. The registrar can refuse or
  "forget" a name, but it cannot point a name at a different address. Names are
  first-come; only the holder of the same key can update a record.

This is a pragmatic compromise on Zooko's triangle: the registrar is a
convenience layer, not a root of trust. If it disappears, the crypto addresses
keep working, and nothing it says can be forged.

## First contact (bootstrap)

The main UX pain: how do you find the **first** neighbour without DNS and a
central server? Options (we use several at once):

- **An invite link / QR from a friend** — the main path; fits F2F and
  invitation-based growth. "Scan a QR → neighbour added along with their
  petname."
- **Built-in bootstrap nodes** — a list of starting points in the build (as in
  Tor / Yggdrasil). The weak spot: they can be blocked, so the list is updatable
  and is not the only path.
- **Local discovery** — mDNS on the LAN, Bluetooth / Wi-Fi Direct nearby (works
  even without internet — important for phases 2–3).

## Optional, for the future: global readable names

"Squaring" the triangle (all three properties at once) is only possible with
blockchain-based registries — Namecoin, ENS, Handshake: a global consensus on who
owns a name. The price is depending on a blockchain, its cost, and its own
centralizing risks. **Not at the start.** If globally unique readable names are
ever wanted, that is a separate subsystem with explicit trade-offs.

## Open questions

- The format and length of crypto addresses (shorter vs more secure).
- A threat model for introductions: how to stop someone from "introducing" a
  substituted address (the introducer's signature? an in-person fingerprint
  check, like Signal's safety number?).
- Whether a global namespace is needed at all, or petnames + web-of-trust cover
  everything.
