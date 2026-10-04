**English** · [Русский](TASKS.ru.md)

> **A historical document (June 2026).** Tasks 1 and 3 were solved differently
> in v0.2: Tor-style circuits, `.ov` services, a SOCKS5 gateway — see
> [docs/running.md](docs/running.md).

# overnet — task collection (handoff)

*Created 2026-06-23. For continuing the work with another AI while the main
assistant was on cooldown. The document is self-contained — it can be read cold.*

---

## 0. Context (read first)

**overnet** is a censorship-resistant decentralized onion mesh network. Goal:
communication that survives national DPI blocking and a closed border. Details
are in `docs/`: `philosophy.md`, `architecture.md`, `threat-model.md`,
`app-protocol.md`, `naming.md`, `plan.md`, `glossary.md` (terms in plain
language).

**Stack:** Rust, a flat cargo workspace, license `AGPL-3.0-only`. Async — `tokio`.

**Golden rules (do NOT break):**
1. **Don't roll your own crypto.** Only proven crates: `x25519-dalek`,
   `chacha20poly1305`, `snow` (Noise), `ed25519-dalek`, `sha2`. The novelty is in
   the construction, not the primitives.
2. **Don't rewrite `ostp`.** overnet depends on `ostp-core` by path
   (`../../ostp/ostp-core`). The lower layers (Reality mimicry, the Noise tunnel,
   padding) come from there. See task 4.
3. **No fake stubs passed off as working code.** If something isn't implemented,
   an honest `TODO`. Everything committed must compile.
4. **`cargo test` must be green.** Tests for every task.
5. **Style:** follow the existing code; see `overnet-core/src/*.rs` as an
   example.

**Building / checking:**
```
cd overnet
cargo test --workspace      # all green at the time of writing
cargo run -p overnet-cli    # generates an identity; server/client commands
```

### Current state (what's done and tested)

Crates (flat in the `overnet/` root):
- `overnet-core` — **done:** `Identity`, `Address`, `Link` (trait), `session`
  (Noise), `onion` (layers). 7 tests green.
- `overnet-link-tcp` — **done:** `TcpLink` (length-framed) + `TcpListenerLink`.
- `overnet-node` — **done:** `serve_echo`, `run_server`, `ping` (client↔server
  echo).
- `overnet-cli` — **done:** `server`/`client` commands.
- `overnet-link-ostp` — **a stub:** only checks linking against `ostp-core`
  (task 4).

### Exact API signatures (so there's no guessing)

```rust
// overnet-core
pub struct Identity;                 // ed25519
impl Identity { fn generate()->Self; fn verifying_key()->ed25519_dalek::VerifyingKey;
                fn address()->Address; fn signing_key()->&SigningKey; }
pub struct Address([u8;32]);         // = sha256(pubkey); Display = "ovn:<hex>"
impl Address { fn from_verifying_key(&VerifyingKey)->Self; fn as_bytes()->&[u8;32]; }

pub enum Error { Link(String), InvalidAddress }   // pub type Result<T>=...

#[async_trait] pub trait Link: Send+Sync {
    async fn send(&self, frame:&[u8])->Result<()>;
    async fn recv(&self)->Result<Vec<u8>>;
    fn mtu(&self)->usize; fn properties(&self)->LinkProps;
}
pub mod link::loopback { pub struct Loopback; impl Loopback{ fn pair()->(Loopback,Loopback);} }

pub mod session {
    pub struct TransportKey { pub private:Vec<u8>, pub public:Vec<u8> }
    impl TransportKey { fn generate()->Result<Self>; }   // Clone
    pub struct Session<L:Link>;
    impl<L:Link> Session<L> {
        async fn initiate(link:L, key:&TransportKey)->Result<Self>;  // client
        async fn respond(link:L, key:&TransportKey)->Result<Self>;   // server
        fn remote_static(&self)->&[u8];
        async fn send(&mut self,&[u8])->Result<()>;
        async fn recv(&mut self)->Result<Vec<u8>>;
    }
}

pub mod onion {
    pub struct OnionKey;                 // x25519
    impl OnionKey { fn generate()->Self; fn public(&self)->[u8;32]; }
    pub fn wrap(path:&[[u8;32]], payload:&[u8])->Result<Vec<u8>>;  // path=[hop1..,dest]
    pub enum Peeled { Forward{ next:[u8;32], inner:Vec<u8> }, Deliver(Vec<u8>) }
    pub fn peel(key:&OnionKey, packet:&[u8])->Result<Peeled>;
}

// overnet-link-tcp
pub struct TcpLink; impl TcpLink { async fn connect(addr:&str)->Result<Self>;
                                   fn from_stream(TcpStream)->Self; }   // impl Link
pub struct TcpListenerLink;
impl TcpListenerLink { async fn bind(addr:&str)->Result<Self>;
                       async fn accept(&self)->Result<TcpLink>;
                       fn local_addr(&self)->Result<SocketAddr>; }
```

---

## TASK 1 — Onion routing A→R→B over `Link` (closes milestone 3 = MVP)

**Goal:** connect the existing `Link` + `onion` into a live path: client A sends
a message to destination B through relay R; **R can't read the payload**, B
receives it.

**Where:** a new module in `overnet-node` (e.g. `src/router.rs`).

**Design:**
- Every node has an `OnionKey` (its onion identity) and a **neighbour table**:
  `next_onion_pub: [u8;32] -> addr: String` (static in the test).
- **The node's relay loop** (for every incoming `Link`):
  1. `frame = link.recv().await?`
  2. `match onion::peel(&my_onion_key, &frame)?`:
     - `Peeled::Forward{ next, inner }` → look up `addr` for `next` in the
       table → `TcpLink::connect(addr)` (or reuse a connection) → `send(&inner)`.
     - `Peeled::Deliver(payload)` → hand the payload to the application (in the
       test — a channel / log).
- **Side A (sender):** `path = [R.onion_pub, B.onion_pub]`,
  `pkt = onion::wrap(&path, payload)?`, `TcpLink::connect(R_addr).send(&pkt)`.
- In the test a node always treats incoming `Link` frames as onion packets (a
  simplification; the real protocol will have a frame type). The transport
  `Link` here is plain TCP (the onion encrypts its own layers; encryption of the
  Link itself comes with ostp in task 4 — orthogonal).

**Acceptance test** (integration, in `overnet-node`):
```
A → R → B over TCP:
  - B listens, peel → Deliver(payload) → puts it in a channel.
  - R listens, peel → Forward → forwards inner to B's address.
  - A: wrap([R_pub,B_pub], b"secret") and sends it to R.
Check: B received b"secret"; on R's side peel returned Forward (not Deliver);
       R does NOT see b"secret" (as in onion::tests).
```

**Notes / pitfalls:**
- This is **one-way** delivery A→B. The return path (a reply) is task 1b (a
  reply onion or an established circuit). Don't mix them.
- Watch connection lifetimes; in the test, connect-per-frame is fine.

### TASK 1b — the return path / circuit (after 1)
Let B reply to A without knowing A's address directly (a reply block / an
established circuit). Design it minimally, describe it in
`docs/architecture.md`, implement + test.

---

## TASK 2 — Trust roots (genesis) and vouchers: "register the owner"

**Goal:** the owner and their devices are the most trusted parties in the
network. Implement trust roots and signed vouchers (the basis of open/vouched
from `architecture.md §2`).

**Where:** a new module `overnet-core/src/trust.rs`.

**Design:**
- **A trust root** = an ed25519 public key from a set **built into the client /
  config** (`TrustRoots(Vec<VerifyingKey>)`). Several roots (not one) — against
  a single point.
- **A device** = an `Identity` (ed25519). The owner's devices are either roots
  themselves or signed directly by a root.
- **A voucher** = the issuer's ed25519 signature over
  `(invitee_pubkey, capabilities, expiry, nonce)`:
  ```rust
  pub struct Voucher { issuer:VerifyingKey, invitee:VerifyingKey,
                       caps:u32, expiry_unix:u64, nonce:[u8;16], sig:Signature }
  impl Voucher {
    fn create(issuer:&SigningKey, invitee:&VerifyingKey, caps:u32, expiry:u64)->Self;
    fn verify(&self)->bool;                 // signature over canonical bytes
  }
  ```
  Reuse `ed25519-dalek` (already in `Identity`); the canonical bytes to sign are
  the fields in a fixed order.
- **Trust evaluation:** a node is "trusted" if there's a valid voucher chain
  from one of the `TrustRoots` (for the MVP — depth 1: a root vouches for a
  device directly; longer chains are the next iteration).

**Acceptance test:** create→verify ok; verify catches forgery and expiry; a
device signed by a root passes `chains_to_root`, a stranger doesn't.

**Honest caveats (to write down in `docs/trust-and-membership.md`, create the
file):** roots are a trust assumption (but multiple, and they only vouch, they
don't read traffic); storing "who vouched for whom" leaks the social graph →
later anonymous credentials (Coconut/Privacy Pass), see the earlier notes in
`architecture.md §2`.

---

## TASK 3 — An access client: a local `overnet://` gateway

**Goal:** let a person get into the network without a browser of our own.

**Where:** a new crate `overnet-gateway` (a binary) + `overnet-node` if needed.

**Design:**
- A local HTTP listener on `127.0.0.1:<port>`.
- A request for `name.ov` (or the path `overnet://name.ov/...`): resolve
  `name.ov` → the service's crypto address (a static table / config for now;
  directory resolution later), build an onion path, send the request (HTTP
  semantics, see `app-protocol.md`), return the response to the browser.
- The service side: a `Service` node answers at its crypto address with content.
- Do NOT build a browser. The user points any browser at the local port; later,
  an OS handler for the `overnet://` scheme.

**Acceptance test:** a local service `hello.ov` serves a page; the gateway
proxies `GET hello.ov/` through the onion (through the nodes from task 1) and
returns the body. The relay doesn't see the content.

**Depends on:** task 1 (the onion route).

---

## TASK 4 — `overnet-link-ostp`: wrap `ostp::ProtocolMachine` as a `Link` (milestone 4)

**Goal:** an internet `Link` with Reality mimicry for phase 1 — a home ↔
servers-abroad test.

**Where:** `overnet-link-ostp` (currently a stub).

**Design:**
- Read `../ostp/ostp-core/src/protocol.rs` (`ProtocolMachine`,
  `ProtocolConfig`, `OstpEvent`, `ProtocolAction`, `OstpState`) and
  `crypto/reality.rs`.
- Open a carrier TCP socket and drive the **sans-io** `ProtocolMachine` over it
  (feed `OstpEvent::Inbound`, execute `ProtocolAction`) to get an established
  tunnel.
- Wrap the established stream in a type implementing `overnet_core::Link`
  (`send`/`recv` over the encrypted, obfuscated channel).
- License: `ostp` and overnet are both AGPLv3 — fine.

**Acceptance test:** two processes, one the `ostp` server side, the other an
`overnet-link-ostp` client; a frame goes there and back through the obfuscated
tunnel. (If bringing up a full `ostp` in a test is hard, start with a
configuration smoke test.)

**Pitfalls:** `ProtocolConfig` needs PSK / Reality parameters; work them out
from the `ostp` code. Don't pass off unfinished work as working.

---

## TASK 5 — A strict onion (Sphinx) and metadata protection (after the MVP)
The current `onion` is simplified (variable length, no replay protection). Move
to Sphinx properties: a fixed packet length, replay protection, padding to
indistinguishability; a mixnet hook (delays / cover) as an option. See
`architecture.md §4`, `glossary.md`. Replace the KDF with HKDF (SHA-256 now —
noted in `onion.rs`).

---

## Order of work (recommendation)
1 → 2 → 3 (this gives a working network with access and trust) → 4 (phase 1
live) → 5. After each task: update the checkboxes in `docs/plan.md` and this
file; `cargo test` green.
