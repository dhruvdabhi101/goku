const $ = id => document.getElementById(id);
const api = window.goku;
let busy = false;
let refreshing = false;
let canRecord = false;
let running = false;
let paused = false;

function notice(message, error = false) {
  $('notice').textContent = message;
  $('notice').className = error ? 'error' : '';
}

function controls() {
  $('start').disabled = busy || running || !canRecord;
  $('stop').disabled = busy || !running;
  $('capture').disabled = busy || !running || paused;
  $('pause').disabled = busy || !running || paused;
  $('resume').disabled = busy || !running || !paused;
}

async function action(fn, message) {
  busy = true;
  controls();
  try { await fn(); notice(message); }
  catch (error) { notice(error.message, true); }
  finally { busy = false; await refresh(); controls(); }
}

function renderPermissions(report) {
  canRecord = report.can_record;
  $('permissions').replaceChildren();
  for (const [kind, title] of [['screen', 'Screen Recording'], ['accessibility', 'Accessibility']]) {
    const card = document.createElement('div'); card.className = 'permission';
    const heading = document.createElement('h3'); heading.textContent = title;
    const value = document.createElement('div'); value.className = 'value';
    const state = report[kind]; value.textContent = state.replaceAll('_', ' ');
    const granted = state === 'granted' || state === 'not_needed';
    if (granted) value.classList.add('granted');
    const request = document.createElement('button'); request.textContent = 'Request'; request.disabled = granted;
    request.onclick = () => action(() => api.requestPermission(kind), 'Permission requested. Finish the system prompt, then refresh or reopen the app.');
    const settings = document.createElement('button'); settings.textContent = 'Open Settings';
    settings.onclick = () => action(() => api.openSettings(kind), 'Enable Goku Capture Test in System Settings. Quit and reopen if the reading stays denied.');
    card.append(heading, value, request, settings); $('permissions').append(card);
  }
}

async function selectFrame(id) {
  try {
    const frame = await api.frame(id);
    $('detail').hidden = false;
    $('metadata').textContent = `${frame.app_name} · ${frame.window_title} · ${frame.image_status}`;
    $('text').textContent = frame.text || 'No extracted text.';
    $('preview').hidden = true;
    $('preview').removeAttribute('src');
    if (frame.image_status === 'ready') {
      $('preview').src = await api.frameImage(id);
      $('preview').hidden = false;
    }
  } catch (error) { notice(error.message, true); }
}

async function refresh() {
  if (refreshing) return;
  refreshing = true;
  try {
    const report = await api.permissions(); renderPermissions(report);
    const status = await api.status(); running = status.running; paused = status.paused;
    const events = await api.events();
    for (const event of events) {
      if (event.error) notice(event.error.message, true);
    }
    $('state').textContent = running ? (paused ? 'Paused' : 'Recording') : (canRecord ? 'Ready' : 'Needs permission');
    const frames = await api.recentFrames();
    $('count').textContent = `${frames.length} recent frames`;
    $('frames').replaceChildren();
    if (!frames.length) { const empty = document.createElement('p'); empty.textContent = 'No frames yet.'; $('frames').append(empty); }
    for (const frame of frames) {
      const button = document.createElement('button'); button.className = 'frame';
      button.textContent = `${frame.app_name || 'Unknown app'} · ${frame.trigger} · ${new Date(frame.captured_at).toLocaleTimeString()}`;
      const small = document.createElement('small'); small.textContent = `${frame.image_status} · ${frame.text_preview || 'No text'}`;
      button.append(small); button.onclick = () => selectFrame(frame.id); $('frames').append(button);
    }
    controls();
  } catch (error) { notice(error.message, true); }
  finally { refreshing = false; }
}

$('refresh').onclick = refresh;
$('start').onclick = () => action(() => api.start(), 'Session started. Switch to a safe app. Capture triggers are active until you stop.');
$('stop').onclick = () => action(() => api.stop(), 'Session stopped. Saved frames remain on this Mac.');
$('pause').onclick = () => action(() => api.pause(), 'Recording paused.');
$('resume').onclick = () => action(() => api.resume(), 'Recording resumed.');
$('capture').onclick = () => action(() => api.captureInFive(), 'Switch to another safe app now. A snapshot will be taken in five seconds.');
$('data').onclick = () => action(() => api.openData(), 'Opened the local capture folder.');
$('relaunch').onclick = () => api.relaunch();
api.onCapture(result => { notice(result.error || `Captured frame ${result.id}.`, Boolean(result.error)); refresh(); });
refresh();
setInterval(() => { if (!busy) refresh(); }, 4000);
