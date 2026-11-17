const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { createRequire } = require('node:module');

const requireInstalled = createRequire(path.join(path.resolve(process.argv[2]), 'package.json'));
const { NativeRecorder } = requireInstalled('goku-capture-sdk');
const data = fs.mkdtempSync(path.join(os.tmpdir(), 'goku-sdk-smoke-'));
try {
  const recorder = new NativeRecorder(data);
  const permissions = JSON.parse(recorder.permissions());
  assert.ok(['granted', 'denied', 'not_needed', 'unknown'].includes(permissions.screen));
  assert.equal(JSON.parse(recorder.status()).running, false);
  assert.deepEqual(JSON.parse(recorder.recentFrames()), []);
  assert.deepEqual(fs.readdirSync(data), []);
  assert.throws(() => recorder.stop(), /not running/);
  console.log('Installed SDK loads its native binary and does not record or write on startup.');
} finally {
  fs.rmSync(data, { recursive: true, force: true });
}
