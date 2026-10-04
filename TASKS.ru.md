[English](TASKS.md) · **Русский**

> **Исторический документ (июнь 2026).** Задачи 1 и 3 решены иначе в v0.2:
> цепи по образцу Tor, сервисы `.ov`, шлюз SOCKS5 — см.
> [docs/ru/running.md](docs/ru/running.md).

# overnet — сборник задач (handoff)

*Создан 2026-06-23. Для продолжения работы с другим ИИ (напр. Gemini 3.1 pro),
пока основной ассистент на cooldown. Документ самодостаточный — читается с холода.*

---

## 0. Контекст (прочитать первым)

**overnet** — censorship-resistant децентрализованная onion-mesh сеть. Цель:
связь, переживающая блокировки ТСПУ и закрытие границы. Подробности — в `docs/`:
`philosophy.md`, `architecture.md`, `threat-model.md`, `app-protocol.md`,
`naming.md`, `plan.md`, `glossary.md` (термины простым языком).

**Стек:** Rust, плоский cargo-workspace, лицензия `AGPL-3.0-only`. Async — `tokio`.

**Золотые правила (НЕ нарушать):**
1. **Не катать свою крипту.** Только проверенные крейты: `x25519-dalek`,
   `chacha20poly1305`, `snow` (Noise), `ed25519-dalek`, `sha2`. Новизна — в
   конструкции, не в примитивах.
2. **Не переписывать `ostp`.** overnet зависит от `ostp-core` по path
   (`../../ostp/ostp-core`). Низ (Reality-мимикрия, Noise-туннель, паддинг) берём
   оттуда. См. задачу 4.
3. **Никаких фейковых заглушек, выдаваемых за рабочее.** Если не реализовано —
   честный `TODO`. Всё, что коммитится, должно компилироваться.
4. **`cargo test` должен быть зелёным.** На каждую задачу — тесты.
5. **Стиль:** комментарии на русском, как в существующем коде; смотри
   `overnet-core/src/*.rs` как образец.

**Как собрать/проверить:**
```
cd overnet
cargo test --workspace      # всё зелёное на момент написания
cargo run -p overnet-cli    # сгенерит личность; команды server/client
```

### Текущее состояние (что уже готово и протестировано)

Крейты (плоско в корне `overnet/`):
- `overnet-core` — **готово:** `Identity`, `Address`, `Link` (trait), `session`
  (Noise), `onion` (слои). 7 тестов зелёные.
- `overnet-link-tcp` — **готово:** `TcpLink` (length-framed) + `TcpListenerLink`.
- `overnet-node` — **готово:** `serve_echo`, `run_server`, `ping` (эхо клиент↔сервер).
- `overnet-cli` — **готово:** команды `server`/`client`.
- `overnet-link-ostp` — **заглушка:** только проверка линковки с `ostp-core` (задача 4).

### Точные сигнатуры API (чтобы не гадать)

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
        async fn initiate(link:L, key:&TransportKey)->Result<Self>;  // клиент
        async fn respond(link:L, key:&TransportKey)->Result<Self>;   // сервер
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

## ЗАДАЧА 1 — Маршрутизация onion A→R→B по `Link` (закрывает Веху 3 = MVP)

**Цель:** соединить готовые `Link` + `onion` в живой путь: клиент A шлёт
сообщение адресату B через релей R; **R не может прочитать payload**, B получает.

**Где:** новый модуль в `overnet-node` (напр. `src/router.rs`).

**Дизайн:**
- У каждого узла есть `OnionKey` (его onion-идентичность) и **таблица соседей**:
  `next_onion_pub: [u8;32] -> addr: String` (статически в тесте).
- **Релей-цикл узла** (на каждом входящем `Link`):
  1. `frame = link.recv().await?`
  2. `match onion::peel(&my_onion_key, &frame)?`:
     - `Peeled::Forward{ next, inner }` → найти `addr` для `next` в таблице →
       `TcpLink::connect(addr)` (или переиспользовать соединение) → `send(&inner)`.
     - `Peeled::Deliver(payload)` → отдать payload приложению (в тесте — в канал/лог).
- **Сторона A (отправитель):** `path = [R.onion_pub, B.onion_pub]`,
  `pkt = onion::wrap(&path, payload)?`, `TcpLink::connect(R_addr).send(&pkt)`.
- В тесте входящие `Link`-кадры узел всегда трактует как onion-пакеты (упрощение;
  в боевом протоколе будет тип кадра). Транспорт-`Link` тут — обычный TCP (onion
  сам шифрует слои; шифрование самого Link добавит ostp в задаче 4 — ортогонально).

**Тест приёмки** (интеграционный, в `overnet-node`):
```
A → R → B по TCP:
  - B слушает, peel → Deliver(payload) → кладёт в канал.
  - R слушает, peel → Forward → пересылает inner на адрес B.
  - A: wrap([R_pub,B_pub], b"secret") и шлёт R.
Проверить: B получил b"secret"; на стороне R peel дал Forward (не Deliver);
           R НЕ видит b"secret" (как в onion::tests).
```

**Заметки/грабли:**
- Это **однонаправленная** доставка A→B. Обратный путь (ответ) — Задача 1b
  (reply-onion или установленная цепь). Не смешивать.
- Следить за временем жизни соединений; в тесте можно connect-на-каждый-кадр.

### ЗАДАЧА 1b — обратный путь / цепь (после 1)
Дать B ответить A, не зная адреса A напрямую (reply-block / established circuit).
Спроектировать минимально, описать в `docs/architecture.md`, реализовать + тест.

---

## ЗАДАЧА 2 — Корни доверия (genesis) и ваучеры: «зарегистрировать владельца»

**Цель:** владелец и его устройства — самые доверенные лица сети. Реализовать
корни доверия и подписанные ваучеры (основа open/vouched из `architecture.md §2`).

**Где:** новый модуль `overnet-core/src/trust.rs`.

**Дизайн:**
- **Корень доверия** = ed25519-публичный ключ из набора, **вшитого в клиент/конфиг**
  (`TrustRoots(Vec<VerifyingKey>)`). Корней несколько (не один) — против single point.
- **Устройство** = `Identity` (ed25519). Устройства владельца либо сами корни,
  либо напрямую подписаны корнем.
- **Ваучер** = ed25519-подпись издателя над `(invitee_pubkey, capabilities, expiry, nonce)`:
  ```rust
  pub struct Voucher { issuer:VerifyingKey, invitee:VerifyingKey,
                       caps:u32, expiry_unix:u64, nonce:[u8;16], sig:Signature }
  impl Voucher {
    fn create(issuer:&SigningKey, invitee:&VerifyingKey, caps:u32, expiry:u64)->Self;
    fn verify(&self)->bool;                 // подпись над каноническими байтами
  }
  ```
  Переиспользовать `ed25519-dalek` (уже в `Identity`); канонические байты для
  подписи — фиксированный порядок полей.
- **Оценка доверия:** узел «доверенный», если есть валидная цепочка ваучеров от
  одного из `TrustRoots` (для MVP — глубина 1: корень напрямую ваучит устройство;
  цепочки длиннее — следующая итерация).

**Тест приёмки:** create→verify ок; verify ловит подделку и просрочку (expiry);
устройство, подписанное корнем, проходит `chains_to_root`, чужое — нет.

**Честные оговорки (записать в `docs/trust-and-membership.md`, создать файл):**
корни — это допущение доверия (но множественные, и они только ваучат, не читают
трафик); хранение «кто кого ваучил» сливает соцграф → позже анонимные удостоверения
(Coconut/Privacy Pass), см. прошлые заметки в `architecture.md §2`.

---

## ЗАДАЧА 3 — Клиент доступа: локальный шлюз `overnet://`

**Цель:** дать человеку зайти в сеть без своего браузера.

**Где:** новый крейт `overnet-gateway` (бинарь) + при необходимости в `overnet-node`.

**Дизайн:**
- Локальный HTTP-листенер на `127.0.0.1:<port>`.
- Запрос на `name.ov` (или путь `overnet://name.ov/...`): резолвить `name.ov` →
  криптоадрес сервиса (пока — статическая таблица/конфиг; directory-резолв позже),
  построить onion-путь, отправить запрос (семантика HTTP, см. `app-protocol.md`),
  вернуть ответ браузеру.
- Сервис-сторона: узел-`Service` отвечает по своему криптоадресу контентом.
- НЕ строить браузер. Пользователь наводит любой браузер на локальный порт; позже —
  OS-обработчик схемы `overnet://`.

**Тест приёмки:** локальный сервис `hello.ov` отдаёт страницу; gateway проксирует
`GET hello.ov/` через onion (через узлы из Задачи 1) и возвращает тело. Релей не
видит контент.

**Зависит от:** Задача 1 (onion-маршрут).

---

## ЗАДАЧА 4 — `overnet-link-ostp`: обернуть `ostp::ProtocolMachine` как `Link` (Веха 4)

**Цель:** интернет-`Link` с Reality-мимикрией для фазы 1 — тест дом(РФ)↔загран-серверы.

**Где:** `overnet-link-ostp` (сейчас заглушка).

**Дизайн:**
- Прочитать `../ostp/ostp-core/src/protocol.rs` (`ProtocolMachine`, `ProtocolConfig`,
  `OstpEvent`, `ProtocolAction`, `OstpState`) и `crypto/reality.rs`.
- Поднять несущий TCP-сокет, гонять по нему **sans-io** `ProtocolMachine`
  (скармливать `OstpEvent::Inbound`, исполнять `ProtocolAction`), получить
  established-туннель.
- Обернуть established-поток в тип, реализующий `overnet_core::Link`
  (`send`/`recv` поверх зашифрованного обфусцированного канала).
- Лицензия: `ostp` и overnet оба AGPLv3 — ок.

**Тест приёмки:** два процесса, один — `ostp`-сервер-сторона, второй —
`overnet-link-ostp` клиент; кадр проходит туда-обратно через обфусцированный туннель.
(Если поднять полноценный `ostp` сложно в тесте — сначала smoke-тест конфигурации.)

**Грабли:** `ProtocolConfig` требует PSK/Reality-параметры; разобраться с ними по
коду `ostp`. Не выдавать недоделанное за рабочее.

---

## ЗАДАЧА 5 — Строгий onion (Sphinx) и защита метаданных (после MVP)
Текущий `onion` — упрощённый (переменная длина, без replay-защиты). Перейти к
Sphinx-свойствам: фиксированная длина пакета, защита от повторов, паддинг до
неразличимости; mixnet-хук (задержки/cover) как опция. См. `architecture.md §4`,
`glossary.md`. Заменить KDF на HKDF (сейчас SHA-256 — пометка в `onion.rs`).

---

## Порядок выполнения (рекомендация)
1 → 2 → 3 (это даёт работающую сеть с доступом и доверием) → 4 (фаза 1 вживую) → 5.
После каждой задачи: обновить чекбоксы в `docs/plan.md` и этот файл; `cargo test` зелёный.
