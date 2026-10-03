// Минимальный браузер overnet на Electron.
//
// Идея: браузер НЕ знает про onion/сеть. Он лишь вешает схему `overnet://` на
// локальный HTTP-шлюз ядра overnet (`overnet gateway`, по умолчанию :8088).
// Так выбор браузера развязан с ядром (см. docs/decisions.md).

const { app, BrowserWindow, protocol, net } = require('electron');

const GATEWAY = 'http://127.0.0.1:8088'; // overnet gateway

// Схему нужно объявить привилегированной ДО app.whenReady().
protocol.registerSchemesAsPrivileged([
  {
    scheme: 'overnet',
    privileges: { standard: true, secure: true, supportFetchAPI: true },
  },
]);

function createWindow() {
  const win = new BrowserWindow({
    width: 1100,
    height: 800,
    webPreferences: { webviewTag: true, contextIsolation: true },
  });
  win.loadFile('index.html');
}

app.whenReady().then(() => {
  // overnet://<host>/<path>  ->  GET http://127.0.0.1:8088/<path>  ->  HTML
  protocol.handle('overnet', async (req) => {
    let pathname = '/';
    try {
      pathname = new URL(req.url).pathname || '/';
    } catch (_) {}

    try {
      const r = await net.fetch(GATEWAY + pathname);
      const body = await r.text();
      return new Response(body, {
        status: r.status,
        headers: { 'content-type': 'text/html; charset=utf-8' },
      });
    } catch (e) {
      return new Response(
        `<body style="font-family:sans-serif;padding:2rem">
           <h1>overnet is unavailable</h1>
           <p>The local gateway <code>${GATEWAY}</code> is not answering.</p>
           <p>Are <code>overnet gateway</code> and the nodes (bootstrap + service) running?</p>
           <pre>${e}</pre>
         </body>`,
        { status: 502, headers: { 'content-type': 'text/html; charset=utf-8' } }
      );
    }
  });

  createWindow();
  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) createWindow();
  });
});

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') app.quit();
});
