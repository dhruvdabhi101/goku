const { parentPort, workerData } = require('node:worker_threads');
const fs = require('node:fs');
const { NativeRecorder } = require('./goku.node');
const filter = fs.existsSync(workerData.filterPath) ? fs.readFileSync(workerData.filterPath, 'utf8') : undefined;
const recorder = new NativeRecorder(workerData.dataDir, filter);
const jsonMethods = new Set(['permissions', 'status', 'events', 'recentFrames', 'frame']);

parentPort.on('message', ({ id, method, args }) => {
  try {
    let value = recorder[method](...args);
    if (jsonMethods.has(method)) value = JSON.parse(value);
    if (method === 'frameImage') value = 'data:image/jpeg;base64,' + Buffer.from(value).toString('base64');
    parentPort.postMessage({ id, value });
  } catch (error) {
    parentPort.postMessage({ id, error: error.message });
  }
});
