const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { _electron: electron } = require('playwright-core');

test('packaged app shows native permission state without starting recording', async () => {
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'goku-electron-test-'));
  const executablePath = process.env.GOKU_TEST_EXECUTABLE || path.join(__dirname, 'dist/Goku Capture Test-darwin-arm64/Goku Capture Test.app/Contents/MacOS/Goku Capture Test');
  const application = await electron.launch({ executablePath, args: [`--user-data-dir=${profile}`], timeout: 30000 });
  try {
    const page = await application.firstWindow();
    await page.locator('#state').filter({ hasText: /Needs permission|Ready/ }).waitFor();
    const report = await page.evaluate(() => window.goku.permissions());
    for (const key of ['screen', 'accessibility', 'input']) {
      assert.ok(['granted', 'denied', 'not_needed', 'unknown'].includes(report[key]));
    }
    assert.equal(report.can_record, report.screen === 'granted' || report.screen === 'not_needed');
    const status = await page.evaluate(() => window.goku.status());
    assert.equal(status.running, false);
    assert.deepEqual(await page.evaluate(() => window.goku.recentFrames()), []);
    assert.equal(await page.locator('#capture').isDisabled(), true);
    assert.equal(await page.locator('#start').isDisabled(), !report.can_record);
    await assert.rejects(page.evaluate(() => window.goku.requestPermission('not-a-permission')), /Unknown permission/);
    await assert.rejects(page.evaluate(() => window.goku.frame(-1)), /Invalid frame id/);
    assert.equal(await page.evaluate(() => typeof window.require), 'undefined');
    const screenshot = process.env.GOKU_SCREENSHOT || path.join(os.tmpdir(), 'goku-electron-test.png');
    await page.screenshot({ path: screenshot });
    console.log(JSON.stringify({ permissions: report, running: status.running, screenshot }));
  } finally {
    await application.close();
    fs.rmSync(profile, { recursive: true, force: true });
  }
});
