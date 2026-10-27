const { app, BrowserWindow, ipcMain, shell, Menu, protocol } = require('electron');
const path = require('path');
const fs = require('fs');
const { execFile } = require('child_process');

// Своя схема app:// — стабильный адрес, чтобы localStorage (база, настройки) не терялся
// между запусками portable-версии, которая распаковывается во временную папку.
protocol.registerSchemesAsPrivileged([
    { scheme: 'app', privileges: { standard: true, secure: true, supportFetchAPI: true } }
]);

if (!app.requestSingleInstanceLock()) app.quit();

const UI_DIR = path.join(__dirname, 'ui');
const MIME = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.css': 'text/css',
               '.json': 'application/json', '.png': 'image/png', '.ico': 'image/x-icon', '.svg': 'image/svg+xml' };

/* ---------- Нативные функции ---------- */
function validHost(h) {
    return typeof h === 'string' && h.length > 0 && h.length <= 253 && !h.startsWith('-') && /^[A-Za-z0-9.-]+$/.test(h);
}

// Настоящий ICMP-пинг через системный ping (права администратора не нужны).
// Онлайн = в ответе есть "TTL=" (одинаково в любой локали Windows).
function pingHost(ip, timeoutMs) {
    return new Promise(resolve => {
        if (!validHost(ip)) return resolve({ ok: false, ms: 0 });
        timeoutMs = Math.min(30000, Math.max(200, parseInt(timeoutMs, 10) || 2000));
        const t0 = Date.now();
        const win = process.platform === 'win32';
        const bin = win ? path.join(process.env.SystemRoot || 'C:\\Windows', 'System32', 'ping.exe') : 'ping';
        const args = win ? ['-n', '1', '-w', String(timeoutMs), ip]
                         : ['-c', '1', '-W', String(Math.ceil(timeoutMs / 1000)), ip];
        execFile(bin, args, { windowsHide: true, timeout: timeoutMs + 2000, encoding: 'latin1' }, (err, stdout) => {
            const out = stdout || '';
            const ok = !err && /ttl=/i.test(out);
            let ms = Date.now() - t0;
            const m = out.match(/[=<]\s*(\d+)\S*\s+TTL=/i);   // "time=2ms TTL=" / "время=2мс TTL="
            if (ok && m) ms = parseInt(m[1], 10);
            resolve({ ok, ms });
        });
    });
}

// hosts.json рядом с exe
function loadHosts() {
    const dirs = [process.env.PORTABLE_EXECUTABLE_DIR, path.dirname(process.execPath), process.cwd()].filter(Boolean);
    for (const d of dirs) {
        try { return fs.readFileSync(path.join(d, 'hosts.json'), 'utf8').replace(/^\uFEFF/, ''); } catch (e) { /* дальше */ }
    }
    return null;
}

function openUrl(url) {
    if (typeof url !== 'string' || !url.startsWith('http://')) throw new Error('only http');
    const host = url.slice(7).replace(/\/+$/, '');
    if (!validHost(host)) throw new Error('bad host');
    return shell.openExternal('http://' + host);
}

ipcMain.handle('native:ping_host', (_e, a) => pingHost(a && a.ip, a && a.timeoutMs));
ipcMain.handle('native:load_hosts', () => loadHosts());
ipcMain.handle('native:open_url', (_e, a) => openUrl(a && a.url));

/* ---------- Окно ---------- */
function createWindow() {
    const win = new BrowserWindow({
        width: 1400, height: 860, minWidth: 1000, minHeight: 600,
        backgroundColor: '#0a0f1e',
        title: 'Мониторинг AP_Babilon-T',
        icon: path.join(__dirname, 'build', 'icon.png'),
        autoHideMenuBar: true,
        webPreferences: { preload: path.join(__dirname, 'preload.js'), contextIsolation: true, nodeIntegration: false, sandbox: true }
    });
    win.webContents.setWindowOpenHandler(() => ({ action: 'deny' }));
    win.webContents.on('will-navigate', e => e.preventDefault());
    win.loadURL('app://local/index.html');
}

app.whenReady().then(() => {
    Menu.setApplicationMenu(null);
    protocol.handle('app', req => {
        let p = decodeURIComponent(new URL(req.url).pathname);
        if (p === '/' || !p) p = '/index.html';
        const full = path.normalize(path.join(UI_DIR, p));
        if (!full.startsWith(UI_DIR)) return new Response('forbidden', { status: 403 });
        try {
            const data = fs.readFileSync(full);
            return new Response(data, { headers: { 'content-type': MIME[path.extname(full).toLowerCase()] || 'application/octet-stream' } });
        } catch (e) {
            return new Response('not found', { status: 404 });
        }
    });
    createWindow();
    app.on('activate', () => { if (!BrowserWindow.getAllWindows().length) createWindow(); });
});

app.on('second-instance', () => {
    const w = BrowserWindow.getAllWindows()[0];
    if (w) { if (w.isMinimized()) w.restore(); w.focus(); }
});
app.on('window-all-closed', () => app.quit());
