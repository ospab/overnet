// overnet browser: настройки по умолчанию поверх Mullvad Browser.
// repack.py дописывает этот файл в конец 000-mullvad-browser.js и
// 001-base-profile.js в browser/omni.ja, поэтому он перекрывает их.

// Всё — через свой шлюз overnet (его запускает браузер, см. overnet.cfg).
// Имена резолвит шлюз: иначе .ov ушёл бы в системный DNS.
pref("network.proxy.type", 1);
pref("network.proxy.socks", "127.0.0.1");
pref("network.proxy.socks_port", 9150);
pref("network.proxy.socks_version", 5);
pref("network.proxy.socks_remote_dns", true);
pref("network.proxy.no_proxies_on", "");
pref("network.proxy.allow_hijacking_localhost", true);
pref("network.proxy.failover_direct", false);

// Свой DoH Mullvad обошёл бы шлюз.
pref("network.trr.mode", 5);
pref("network.trr.uri", "");
pref("network.trr.default_provider_uri", "");

// Стартовая страница — browser.ov, её отдаёт сам шлюз.
pref("browser.startup.homepage", "http://browser.ov/");
pref("browser.startup.page", 1);
pref("browser.base-browser-support-url", "https://github.com/ospab/overnet/blob/master/docs/running.md");
pref("app.feedback.baseURL", "https://github.com/ospab/overnet/issues");

// «search.ov» в адресной строке — сайт, а не поисковый запрос.
pref("browser.fixup.domainsuffixwhitelist.ov", true);
pref("browser.search.suggest.enabled", false);
pref("browser.urlbar.suggest.searches", false);

// Сайты .ov — http (шифрует сама сеть). HTTPS-only сломал бы их, поэтому
// HTTPS-first: обычные сайты всё равно открываются по https.
pref("dom.security.https_only_mode", false);
pref("dom.security.https_only_mode_pbm", false);
pref("dom.security.https_first", true);
pref("dom.security.https_first_pbm", true);
// WebCrypto нужен почте; он есть только в «безопасном контексте».
pref("dom.securecontext.allowlist", "mail.ov");

// Обновления Mullvad вернули бы их брендинг: обновляется сам overnet browser.
pref("app.update.auto", false);
pref("app.update.enabled", false);
pref("app.update.url.manual", "https://github.com/ospab/overnet/releases");
pref("app.update.url.details", "https://github.com/ospab/overnet/releases");

// Не показывать страницу Mullvad «что нового» после обновления.
pref("browser.startup.homepage_override.mstone", "ignore");
pref("startup.homepage_welcome_url", "");
pref("startup.homepage_override_url", "");
