const { app, BrowserWindow, ipcMain, shell } = require('electron');
const { Worker } = require('node:worker_threads');
const path = require('node:path');

let window;
let worker;
let nextId = 0;
let captureTimer;
let quitting = false;
const pending = new Map();
const methods = new Set(['permissions', 'requestPermission', 'openSettings', 'start', 'stop', 'pause', 'resume', 'captureNow', 'status', 'events', 'recentFrames', 'frame', 'frameImage']);

function invoke(method, args = []) {
  return new Promise((resolve, reject) => {
    const id = ++nextId;
    const timeout = setTimeout(() => {
      pending.delete(id);
      reject(new Error('Native operation timed out. Quit and reopen the app.'));
    }, 35000);
    pending.set(id, { resolve, reject, timeout });
    worker.postMessage({ id, method, args });
  });
}

function rejectPending(error) {
  for (const { reject, timeout } of pending.values()) {
    clearTimeout(timeout);
    reject(error);
  }
  pending.clear();
}

app.whenReady().then(() => {
  worker = new Worker(path.join(__dirname, 'native-worker.cjs'), {
    workerData: {
      dataDir: path.join(app.getPath('userData'), 'captures'),
      filterPath: path.join(app.getPath('userData'), 'filter.json'),
    },
  });
  worker.on('message', ({ id, value, error }) => {
    const task = pending.get(id);
    if (!task) return;
    clearTimeout(task.timeout);
    pending.delete(id);
    if (error) task.reject(new Error(error));
    else task.resolve(value);
  });
  worker.on('error', rejectPending);
  worker.on('exit', () => rejectPending(new Error('Native worker exited. Quit and reopen the app.')));
  window = new BrowserWindow({
    width: 1040, height: 800, minWidth: 780, minHeight: 600,
    title: 'Goku Capture Test', backgroundColor: '#11151c',
    webPreferences: { preload: path.join(__dirname, 'preload.cjs'), contextIsolation: true, nodeIntegration: false, sandbox: true },
  });
  window.webContents.setWindowOpenHandler(() => ({ action: 'deny' }));
  window.webContents.on('will-navigate', (event) => event.preventDefault());
  const trusted = (event) => {
    if (event.sender !== window.webContents || event.senderFrame !== window.webContents.mainFrame) throw new Error('Untrusted sender');
  };
  ipcMain.handle('sdk', async (event, method, arg) => {
    trusted(event);
    if (!methods.has(method)) throw new Error('Unknown method');
    if (method === 'requestPermission' || method === 'openSettings') {
      if (!['screen', 'accessibility', 'input'].includes(arg)) throw new Error('Unknown permission');
      return invoke(method, [arg]);
    }
    if (method === 'frame' || method === 'frameImage') {
      if (!Number.isSafeInteger(arg) || arg < 1 || arg > 2147483647) throw new Error('Invalid frame id');
      return invoke(method, [arg]);
    }
    return invoke(method);
  });
  ipcMain.handle('capture-in-five', (event) => {
    trusted(event);
    if (captureTimer) throw new Error('A delayed capture is already pending');
    captureTimer = setTimeout(async () => {
      captureTimer = undefined;
      try {
        const id = await invoke('captureNow');
        if (!window.isDestroyed()) window.webContents.send('capture-result', { id });
      } catch (error) {
        if (!window.isDestroyed()) window.webContents.send('capture-result', { error: error.message });
      }
    }, 5000);
  });
  ipcMain.handle('open-data', (event) => { trusted(event); return shell.openPath(path.join(app.getPath('userData'), 'captures')); });
  ipcMain.handle('relaunch', (event) => { trusted(event); app.relaunch(); app.quit(); });
  window.loadFile('index.html');
});

app.on('window-all-closed', () => app.quit());
app.on('before-quit', (event) => {
  if (quitting || !worker) return;
  event.preventDefault();
  quitting = true;
  clearTimeout(captureTimer);
  Promise.race([invoke('stop').catch(() => {}), new Promise(resolve => setTimeout(resolve, 5000))])
    .finally(() => { worker.terminate(); app.quit(); });
});
