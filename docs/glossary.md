**English** · [Русский](ru/glossary.md)

# Glossary

*Version 0.1 — 2026-06-23. In plain language. Come back here when another
document uses a word you don't know.*

Ordered from "what is this at all" to the details. Each term: what it is + why
it matters for overnet.

---

## Blocking

**DPI** — equipment that reads passing traffic and decides whether to let it
through or cut it. A national DPI system (Russia's is called TSPU) is overnet's
main adversary.

**SNI blocking** — in ordinary TLS the site name (`example.com`) travels **in
plain text**, and DPI blocks by it. Today it's the censor's tool number one.

**Whitelist mode** — the endgame of censorship: only what DPI *positively
recognizes* as permitted gets through; everything else is dropped. It kills any
overlay that merely "looks like random ciphertext". Hence the need for mimicry
or a medium of our own.

**Reality** — a technique for disguising traffic as genuine TLS to a genuine
popular site, so DPI can't tell the difference. Used in `ostp` (phase 1).

**Traffic analysis** — deanonymization without reading the content: by packet
sizes, timing, volume. Encryption hides *what*, but not *when and how much*.

---

## Encryption (the building blocks)

**Symmetric encryption** — one key both encrypts and decrypts. Fast. The
problem: how to get the key to the other side over an open channel.

**Asymmetric encryption** — a key pair: a public key (give it to everyone) and a
private key (only you have it). Solves the key-delivery problem. Slow, so it's
used to encrypt not the data but a small symmetric key (see hybrid scheme).

**Hybrid scheme (KEM/DEM)** — the standard: agree on a symmetric key with
asymmetric crypto → encrypt the data with it, fast. All real-world crypto works
this way. Data and headers are never encrypted with raw RSA.

**Diffie–Hellman / X25519** — a way for two parties to **agree on a shared
secret over an open channel** without sending the secret itself. X25519 is the
modern, fast elliptic-curve version (32-byte keys). Our choice.

**AEAD (e.g. ChaCha20-Poly1305)** — encryption + built-in **integrity check** in
one: if someone altered the ciphertext, decryption honestly fails (instead of
producing garbage). Our choice for the payload.

**Noise Protocol Framework** — a ready-made "kit" for building correct
handshakes out of these blocks. WireGuard is built on it. We use it so we **don't
roll our own crypto**.

**Don't roll your own crypto** — rule #1: primitives are taken proven; the
novelty is in the construction (the protocol), not a home-made cipher.
Home-made ciphers get broken.

---

## Routing and privacy

**Onion routing** — a message is encrypted in layers, like an onion. Each hop
peels its layer and learns **only the next hop** — never the whole picture.
overnet's privacy core.

**Sphinx** — a strict, battle-tested onion packet format: fixed length (the
route length doesn't leak), one X25519 per hop (no trial decryption), replay
protection. We take it instead of the naive "try to decrypt → failed → flood".

**Ntor** — Tor's circuit handshake: the client proves nothing about itself, and
only the holder of a relay's key can complete it. overnet v0.2 uses it for every
hop of a circuit.

**Correlation attack (traffic confirmation)** — if the adversary sees **the
entry and the exit** of the same circuit, it can link them by timing and volume
**without decrypting anything**. The main deanonymization vector. Onion routing
alone doesn't protect against it.

**Mixnet** — nodes don't forward a packet right away; they **collect, shuffle
and release packets out of order**, plus send dummy traffic. That breaks timing
correlation. The price is latency. Onion routing is a mixnet with the shuffling
removed for speed; "improving the onion" means bringing it back. Examples:
Loopix, Nym.

**Cover traffic** — a constant stream of dummies indistinguishable from real
messages; hides even the *fact* that something was sent.

**Guard (entry guard)** — a fixed set of entry points you stick with for a long
time, instead of a random entry every time (otherwise sooner or later you hit a
hostile entry). Ours are **personal and hidden**, not a network-wide public
list.

---

## Trust and addresses

**Sybil attack** — the adversary cheaply creates **many fake identities /
nodes**. For us identity = a key pair = free, so Sybil is especially easy. The
root of the trust problem.

**PoW (Proof of Work)** — a puzzle that is **expensive to solve and cheap to
check** (find a number so that the hash starts with N zeros). Makes creating a
node cost something → Sybil gets more expensive.

**F2F / web-of-trust** — you link directly only with people you **know
personally**. Structurally limits the adversary's share of your surroundings.
For us, an optional hard tier.

**Cryptographic addressing** — a node's address is derived from its public key.
Self-assigned (no registry needed), self-authenticating (can't be forged), not
centrally revocable.

**Petnames** — local nicknames you assign to contacts yourself (a human-readable
layer over an ugly crypto address). Your "alice" ≠ my "alice" — no global
uniqueness, but no central DNS either.

**Zooko's triangle** — a name can be human-readable, secure and decentralized;
classically, **pick two of three**. See naming.md.

---

## Network and medium

**Transport-agnostic** — the core doesn't care *what* the link is (TCP, Wi-Fi,
LoRa, Bluetooth, a USB stick). overnet's key architectural property: when the
internet is choked, everything moves to a physical medium without a rewrite.

**Link** — a single frame-delivery channel between two neighbouring nodes. An
abstraction that can have any medium underneath.

**Mesh network** — nodes connect many-to-many and relay for each other, with no
central node.

**NAT traversal / hole punching** — tricks to establish a direct p2p connection
when both parties are behind a provider's NAT/CGNAT (otherwise they can't "see"
each other directly).

**Sneakernet** — a "network on sneakers": carrying data physically (USB stick,
QR), asynchronously. The least blockable and the slowest transport.

**Exit node** — a node through which traffic leaves for the regular internet
(the clearnet). In overnet exits are **optional and off by default**: a
destination is normally inside the network, so the usual exit-node problems
don't arise. A relay becomes an exit only when its operator turns it on.

**Anonymity set** — how many participants look equally suspicious. The more
honest users, the stronger your anonymity: "anonymity loves company".
