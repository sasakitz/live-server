const streamId = decodeURIComponent(window.location.pathname.replace(/^\/watch\//, ''));
let session = null;
let startedAt = null;
let elapsedTimer = null;
let playbackGeneration = 0;
let leaving = false;
let streamLive = false;
let reconnectNeeded = false;

function showPlayButton(reconnect = false) {
  const button = document.getElementById('play-btn');
  button.textContent = reconnect ? '再接続する' : '再生する';
  button.hidden = false;
  showControls();
}

async function playVideo(generation = playbackGeneration) {
  try {
    // ミュート解除などのクリックからは、awaitより前にplay()を呼ぶ。
    await document.getElementById('video').play();
    if (generation !== playbackGeneration || leaving || reconnectNeeded) return;
    document.getElementById('play-btn').hidden = true;
    setOverlay('');
    scheduleHideControls();
  } catch {
    if (generation !== playbackGeneration || leaving) return;
    showPlayButton(reconnectNeeded);
  }
}

function setOverlay(text) {
  const overlay = document.getElementById('overlay');
  overlay.textContent = text;
  overlay.hidden = !text;
}

function renderMeta(meta) {
  document.title = `${meta.title} | μTube`;
  document.getElementById('title').textContent = meta.title;
  document.getElementById('description').textContent = meta.description || '';
  document.getElementById('viewer-count').textContent = meta.is_live ? `👁 ${meta.viewer_count}人視聴中` : '';
  document.getElementById('live-badge').innerHTML = meta.is_live
    ? '<span class="badge badge--live"><span class="badge__dot"></span>LIVE</span>'
    : '<span class="badge badge--offline">オフライン</span>';

  startedAt = meta.started_at;
  clearInterval(elapsedTimer);
  if (startedAt) {
    const tick = () => { document.getElementById('elapsed').textContent = formatRelativeStart(startedAt); };
    tick();
    elapsedTimer = setInterval(tick, 1000);
  } else {
    document.getElementById('elapsed').textContent = '';
  }
}

async function startPlayback(generation) {
  const video = document.getElementById('video');
  try {
    const candidate = await startWhep(streamId, video, (state) => {
      if (generation !== playbackGeneration || leaving) return;
      if (state === 'failed' || state === 'disconnected' || state === 'closed') {
        reconnectNeeded = true;
        setOverlay('接続が切れました。再接続してください。');
        showPlayButton(true);
      }
    }, () => generation === playbackGeneration && !leaving);
    // 配信終了・ページ離脱・別のloadが先行した場合、遅れて完成した接続を残さない。
    if (generation !== playbackGeneration || leaving) {
      await candidate.stop();
      return;
    }
    session = candidate;
    if (reconnectNeeded) return;
    setOverlay('');
    await playVideo(generation);
  } catch (e) {
    if (generation === playbackGeneration && !leaving) {
      reconnectNeeded = true;
      setOverlay(`視聴を開始できませんでした: ${e.message}`);
      showPlayButton(true);
    }
  }
}

async function load() {
  if (leaving) return;
  const generation = ++playbackGeneration;
  reconnectNeeded = false;
  document.getElementById('play-btn').hidden = true;
  const previous = session;
  session = null;
  if (previous) await previous.stop();
  if (generation !== playbackGeneration || leaving) return;
  let meta;
  try {
    meta = await api(`/api/streams/${encodeURIComponent(streamId)}`);
  } catch (e) {
    if (generation !== playbackGeneration || leaving) return;
    document.getElementById('title').textContent = '視聴できません';
    setOverlay(/ログイン/.test(e.message) ? '視聴にはログインが必要です' : e.message);
    reconnectNeeded = true;
    showPlayButton(true);
    return;
  }
  if (generation !== playbackGeneration || leaving) return;
  renderMeta(meta);
  streamLive = meta.is_live;
  if (meta.is_live) await startPlayback(generation);
  else setOverlay('配信は行われていません');
}

const playBtn = document.getElementById('play-btn');
playBtn.addEventListener('click', async () => {
  if (reconnectNeeded) await load();
  else await playVideo();
});

const muteBtn = document.getElementById('mute-btn');
const volumeSlider = document.getElementById('volume-slider');

function updateMuteBtn() {
  const video = document.getElementById('video');
  const isMuted = video.muted || video.volume === 0;
  muteBtn.textContent = isMuted ? '音声をオン' : 'ミュート';
  muteBtn.setAttribute('aria-label', isMuted ? '音声をオンにする' : '音声をミュートする');
}

muteBtn.addEventListener('click', () => {
  const video = document.getElementById('video');
  video.muted = !video.muted;
  if (!video.muted && video.volume === 0) {
    video.volume = 1;
    volumeSlider.value = 100;
  }
  updateMuteBtn();
  if (!video.muted) playVideo();
});

volumeSlider.addEventListener('input', () => {
  const video = document.getElementById('video');
  video.volume = Number(volumeSlider.value) / 100;
  video.muted = video.volume === 0;
  updateMuteBtn();
  if (!video.muted && video.paused) playVideo();
});

// iOS等ではvolumeへの代入が反映されない。UA判定せず実際の対応を確認する。
function supportsVolumeControl(video) {
  const original = video.volume;
  const probe = original === 0.5 ? 1 : 0.5;
  try {
    video.volume = probe;
    return video.volume === probe;
  } catch {
    return false;
  } finally {
    try { video.volume = original; } catch { /* 端末の音量操作に任せる。 */ }
  }
}

document.getElementById('video').addEventListener('loadedmetadata', () => {
  const video = document.getElementById('video');
  muteBtn.hidden = false;
  volumeSlider.hidden = !supportsVolumeControl(video);
  volumeSlider.value = Math.round(video.volume * 100);
  updateMuteBtn();
  fullscreenBtn.hidden = !(document.fullscreenEnabled || video.webkitEnterFullscreen);
});

document.getElementById('video').addEventListener('volumechange', updateMuteBtn);
document.getElementById('video').addEventListener('pause', () => {
  if (streamLive && session && !leaving && !reconnectNeeded) showPlayButton();
});
document.getElementById('video').addEventListener('playing', () => {
  if (!streamLive || !session || leaving || reconnectNeeded) return;
  playBtn.hidden = true;
  setOverlay('');
  scheduleHideControls();
});

const fullscreenBtn = document.getElementById('fullscreen-btn');
fullscreenBtn.addEventListener('click', () => {
  const video = document.getElementById('video');
  // iOS Safari(iPhone)はvideo以外の要素のFullscreen APIに対応していないため、
  // video自身のネイティブ全画面再生(webkitEnterFullscreen)にフォールバックする。
  if (video.webkitEnterFullscreen && !document.fullscreenEnabled) {
    video.webkitEnterFullscreen();
    return;
  }
  const wrap = document.getElementById('player-wrap');
  if (!document.fullscreenElement) {
    wrap.requestFullscreen().catch(() => {});
  } else {
    document.exitFullscreen().catch(() => {});
  }
});

// コントロールバー(音量/ミュート/全画面)は3秒間無操作だと隠す。
const playerWrap = document.getElementById('player-wrap');
const playerControls = document.querySelector('.player-controls');
let hideControlsTimer = null;

function showControls() {
  playerControls.classList.remove('is-hidden');
  scheduleHideControls();
}

function scheduleHideControls() {
  clearTimeout(hideControlsTimer);
  // 自動再生が拒否された時や再接続待ちには、操作の入口を消さない。
  if (!playBtn.hidden) return;
  hideControlsTimer = setTimeout(() => playerControls.classList.add('is-hidden'), 3000);
}

['mousemove', 'mouseenter', 'touchstart', 'click', 'keydown'].forEach((evt) => {
  playerWrap.addEventListener(evt, showControls);
});
// コントロールバー上にいる間(スライダー操作中など)は隠さない。
playerControls.addEventListener('mouseenter', () => clearTimeout(hideControlsTimer));
playerControls.addEventListener('mouseleave', scheduleHideControls);
showControls();

window.addEventListener('pagehide', () => {
  leaving = true;
  playbackGeneration++;
  if (session) session.stop();
  session = null;
  clearInterval(elapsedTimer);
});
window.addEventListener('pageshow', (event) => {
  if (event.persisted) { leaving = false; load(); }
});

function resumePlayback() {
  if (leaving || document.visibilityState === 'hidden' || !streamLive) return;
  const state = session?.pc?.connectionState;
  if (reconnectNeeded || ['disconnected', 'failed', 'closed'].includes(state)) {
    load();
  } else if (session && document.getElementById('video').paused) {
    playVideo();
  }
}
document.addEventListener('visibilitychange', resumePlayback);
window.addEventListener('online', resumePlayback);

const playbackInfo = document.getElementById('playback-info');
let statsTimer = null;
let statsSession = null;
let statsSamples = new Map();
let statsReading = false;

async function updatePlaybackStats() {
  if (!playbackInfo.open || leaving || document.visibilityState === 'hidden' || statsReading) return;
  const output = document.getElementById('playback-stats');
  const current = session;
  if (!current) { output.textContent = '視聴接続を待っています。'; return; }
  if (statsSession !== current) { statsSession = current; statsSamples = new Map(); }
  statsReading = true;
  try {
    const result = await readPlaybackStats(current.pc, statsSamples);
    if (session !== current || leaving || !playbackInfo.open) return;
    statsSamples = result.samples;
    output.textContent = result.text;
  } catch {
    if (session === current && !leaving) output.textContent = 'この端末では再生情報を取得できませんでした。';
  } finally {
    statsReading = false;
  }
}

function schedulePlaybackStats() {
  clearInterval(statsTimer);
  statsTimer = null;
  statsSamples = new Map();
  if (!playbackInfo.open || leaving || document.visibilityState === 'hidden') return;
  updatePlaybackStats();
  statsTimer = setInterval(updatePlaybackStats, 2000);
}
playbackInfo.addEventListener('toggle', schedulePlaybackStats);
document.addEventListener('visibilitychange', schedulePlaybackStats);
window.addEventListener('pagehide', () => { clearInterval(statsTimer); statsTimer = null; });
window.addEventListener('pageshow', schedulePlaybackStats);

// 通知用WebSocketだけが再接続した場合、正常な映像接続は維持する。
async function resyncStream() {
  if (leaving) return;
  const generation = playbackGeneration;
  try {
    const meta = await api(`/api/streams/${encodeURIComponent(streamId)}`);
    if (generation !== playbackGeneration || leaving) return;
    if (session?.pc?.connectionState === 'connected' && !reconnectNeeded
        && meta.is_live && meta.started_at === startedAt) {
      renderMeta(meta);
    } else {
      await load();
    }
  } catch {
    if (generation === playbackGeneration && !leaving) await load();
  }
}

connectEvents((event) => {
  if (event.type === 'resync') { resyncStream(); return; }
  if (leaving || event.stream_id !== streamId) return;
  if (event.type === 'stream_online') {
    load();
  } else if (event.type === 'stream_offline') {
    streamLive = false;
    reconnectNeeded = false;
    playBtn.hidden = true;
    playbackGeneration++;
    if (session) session.stop();
    session = null;
    setOverlay('配信は終了しました');
    document.getElementById('viewer-count').textContent = '';
    document.getElementById('live-badge').innerHTML = '<span class="badge badge--offline">オフライン</span>';
    clearInterval(elapsedTimer);
    document.getElementById('elapsed').textContent = '';
  } else if (event.type === 'viewer_count') {
    document.getElementById('viewer-count').textContent = `👁 ${event.count}人視聴中`;
  }
});

load();
