# overnet-electron-browser

Минимальный браузер overnet на Electron (Chromium). Вешает схему `overnet://` на
локальный HTTP-шлюз ядра overnet — сам браузер про onion/сеть ничего не знает.

## Запуск

Сначала подними ядро overnet (в отдельных окнах), включая шлюз на `:8088`:

```
overnet bootstrap 127.0.0.1:8080
overnet service   0.0.0.0:4040 127.0.0.1:8080
overnet gateway   127.0.0.1:8088 127.0.0.1:8080
```

(Ветку `gateway` в `overnet-cli` нужно добавить — см. ниже.)

Потом браузер:

```
cd electron-browser
npm install      # скачает Electron (~200 МБ, один раз)
npm start
```

В адресной строке: `overnet://search.ov/` → страница от твоего service-узла.

## Ветка `gateway` для overnet-cli

```rust
Some("gateway") => {
    let bind = args.get(2).cloned().unwrap_or_else(|| "127.0.0.1:8088".into());
    let bootstrap = args.get(3).cloned().unwrap_or_else(|| "127.0.0.1:8080".into());
    println!("gateway http://{bind} -> bootstrap {bootstrap}");
    let _ = overnet_node::web::run_gateway(&bind, bootstrap).await;
}
```

## Как это устроено

```
overnet://search.ov/path
   │  (Electron protocol.handle)
   ▼
GET http://127.0.0.1:8088/path        ← локальный шлюз (overnet_node::web::run_gateway)
   │  (bootstrap → onion → service)
   ▼
HTML от service-узла → рендер в webview
```

## TODO

- Авто-запуск ядра overnet как сайдкара из `main.js` (сейчас запускается отдельно).
- Вкладки, история, индикатор соединения.
- Резолв `name.ov → pubkey` через каталог (сейчас берётся первый service-узел).
