**English** · [Русский](ru/deploy.md)

> **Outdated.** These are instructions for v0.1 (`overnet legacy …`). For
> installing relays, sites and the gateway today, see
> [running.md](running.md#install).

# Deploying overnet relays (v0.1)

To launch an MVP network you need to run 3 relays on your servers abroad.

## 1. Build (for Linux servers)

Since you're on Windows, it's easiest to build `overnet-cli` directly on the
Linux servers, or with `cross`:
```bash
cargo build --release -p overnet-cli
```
The binary ends up in `target/release/overnet-cli`. Copy it to all 3 servers.

## 2. Keys and addresses
Each relay generates a random key (onion pubkey) on startup; alternatively you
can change the code to read the key from a file (for a persistent onion
address). In the MVP, nodes generate a new key on startup and print it.

## 3. Starting the servers (topology A -> R1 -> R2 -> Service)

We set up 3 servers.

**Server 1 (Service / end node):**
On server 1 (IP: `SERVER_1_IP`):
```bash
./overnet-cli service 0.0.0.0:4040
```
> Copy its `onion pubkey`.

**Server 2 (Relay 2):**
On server 2 (IP: `SERVER_2_IP`):
```bash
./overnet-cli relay 0.0.0.0:4040 <SERVER_1_PUBKEY>@SERVER_1_IP:4040
```
> Copy its `onion pubkey`. (This relay forwards packets to Server 1.)

**Server 3 (Relay 1):**
On server 3 (IP: `SERVER_3_IP`):
```bash
./overnet-cli relay 0.0.0.0:4040 <SERVER_2_PUBKEY>@SERVER_2_IP:4040
```
> Copy its `onion pubkey`. (This relay forwards packets to Server 2.)

## 4. The client on your PC
You now have the path `Relay 1 -> Relay 2 -> Service` and know the `pubkey` of
each node.

In `overnet-browser` (or a future client) you would enter the routing path.
While the browser was being finished, this could be tested through the Rust API
of `overnet-browser`, giving it the `pubkey` of all three nodes. (For the Tauri
UI these pubkeys had to be added to the client configuration.)
