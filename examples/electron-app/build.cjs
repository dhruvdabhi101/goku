const { execFileSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const { downloadArtifact } = require('@electron/get');
const root = path.resolve(__dirname, '../..');
const targets = {
  'darwin-arm64': { library: 'libgoku_node.dylib' },
  'win32-x64': { library: 'goku_node.dll' },
};

async function build() {
  const key = `${process.platform}-${process.arch}`;
  const target = targets[key];
  if (!target) throw new Error(`This build script targets Apple Silicon macOS and Windows x64, not ${key}`);
  execFileSync('cargo', ['build', '-p', 'goku-node'], { cwd: root, stdio: 'inherit' });
  fs.copyFileSync(path.join(root, 'target/debug', target.library), path.join(__dirname, 'goku.node'));
  const archive = await downloadArtifact({ version: require('./package.json').devDependencies.electron, platform: process.platform, arch: process.arch, artifactName: 'electron' });
  const dist = path.join(__dirname, 'dist');
  fs.mkdirSync(dist, { recursive: true });
  const staging = fs.mkdtempSync(path.join(dist, '.goku-bundle-'));
  try {
    const bundle = process.platform === 'darwin' ? buildMacBundle(archive, staging) : buildWindowsFolder(archive, staging);
    copyAppFiles(process.platform === 'darwin' ? path.join(bundle, 'Contents/Resources/app') : path.join(bundle, 'resources/app'));
    const outputDir = path.join(__dirname, `dist/Goku Capture Test-${key}`);
    fs.rmSync(outputDir, { recursive: true, force: true });
    fs.mkdirSync(outputDir, { recursive: true });
    const destination = path.join(outputDir, path.basename(bundle));
    fs.renameSync(bundle, destination);
    console.log(destination);
  } finally { fs.rmSync(staging, { recursive: true, force: true }); }
}

function buildMacBundle(archive, staging) {
  execFileSync('/usr/bin/ditto', ['-x', '-k', archive, staging]);
  const bundle = path.join(staging, 'Goku Capture Test.app');
  fs.renameSync(path.join(staging, 'Electron.app'), bundle);
  const contents = path.join(bundle, 'Contents');
  fs.renameSync(path.join(contents, 'MacOS/Electron'), path.join(contents, 'MacOS/Goku Capture Test'));
  const plist = path.join(contents, 'Info.plist');
  for (const [key, value] of Object.entries({
    CFBundleExecutable: 'Goku Capture Test', CFBundleName: 'Goku Capture Test',
    CFBundleDisplayName: 'Goku Capture Test', CFBundleIdentifier: 'dev.goku.capture-test',
    CFBundleShortVersionString: '0.1.0',
  })) execFileSync('/usr/libexec/PlistBuddy', ['-c', `Set :${key} ${value}`, plist]);
  for (const [key, value] of Object.entries({
    NSAccessibilityUsageDescription: 'Read focused-window text for local capture and redaction.',
    NSScreenCaptureUsageDescription: 'Capture the focused app after you start a session.',
  })) execFileSync('/usr/libexec/PlistBuddy', ['-c', `Add :${key} string ${value}`, plist]);
  const defaultAsar = path.join(contents, 'Resources/default_app.asar');
  if (fs.existsSync(defaultAsar)) fs.unlinkSync(defaultAsar);
  execFileSync('codesign', ['--force', '--deep', '--sign', '-', '--identifier', 'dev.goku.capture-test', bundle], { stdio: 'inherit' });
  execFileSync('codesign', ['--verify', '--deep', '--strict', bundle], { stdio: 'inherit' });
  return bundle;
}

function buildWindowsFolder(archive, staging) {
  const folder = path.join(staging, 'Goku Capture Test');
  fs.mkdirSync(folder);
  execFileSync('tar', ['-xf', archive, '-C', folder]);
  fs.renameSync(path.join(folder, 'electron.exe'), path.join(folder, 'Goku Capture Test.exe'));
  return folder;
}

function copyAppFiles(appDir) {
  fs.mkdirSync(appDir, { recursive: true });
  for (const file of ['package.json', 'main.cjs', 'native-worker.cjs', 'preload.cjs', 'renderer.js', 'index.html', 'style.css', 'goku.node']) {
    fs.copyFileSync(path.join(__dirname, file), path.join(appDir, file));
  }
}
build().catch(error => { console.error(error.message); process.exitCode = 1; });
