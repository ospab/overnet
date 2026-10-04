**English** · [Русский](README.ru.md)

> **Outdated.** The overnet browser is now Gecko-based — overnet browser (a
> repackaged Mullvad Browser); see [docs/running.md](../docs/running.md#the-browser)
> and the [decision log](../docs/decisions.md).

# overnet-electron-browser

A minimal overnet browser on Electron (Chromium). It maps the `overnet://`
scheme onto the overnet core's local HTTP gateway — the browser itself knows
nothing about onions or the network.

## Running

First start the overnet core (in separate windows), including the gateway on
`:8088`:

```
overnet bootstrap 127.0.0.1:8080
overnet service   0.0.0.0:4040 127.0.0.1:8080
overnet gateway   127.0.0.1:8088 127.0.0.1:8080
```

(The `gateway` branch has to be added to `overnet-cli` — see below.)

Then the browser:

```
cd electron-browser
npm install      # downloads Electron (~200 MB, once)
npm start
```

In the address bar: `overnet://search.ov/` → a page from your service node.

## The `gateway` branch for overnet-cli

```rust
Some("gateway") => {
    let bind = args.get(2).cloned().unwrap_or_else(|| "127.0.0.1:8088".into());
    let bootstrap = args.get(3).cloned().unwrap_or_else(|| "127.0.0.1:8080".into());
    println!("gateway http://{bind} -> bootstrap {bootstrap}");
    let _ = overnet_node::web::run_gateway(&bind, bootstrap).await;
}
```

## How it works

```
overnet://search.ov/path
   │  (Electron protocol.handle)
   ▼
GET http://127.0.0.1:8088/path        ← local gateway (overnet_node::web::run_gateway)
   │  (bootstrap → onion → service)
   ▼
HTML from the service node → rendered in the webview
```

## TODO

- Start the overnet core as a sidecar from `main.js` (it's started separately
  now).
- Tabs, history, a connection indicator.
- Resolve `name.ov → pubkey` through the directory (currently the first service
  node is taken).
