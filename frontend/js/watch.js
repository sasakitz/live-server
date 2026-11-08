const streamId = decodeURIComponent(window.location.pathname.replace(/^\/watch\//, ''));
let session = null;
let startedAt = null;
let elapsedTimer = null;
let playbackGeneration = 0;
let leaving = false;

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
        setOverlay('接続が切れました。再読み込みしてください。');
      }
    });
    // 配信終了・ページ離脱・別のloadが先行した場合、遅れて完成した接続を残さない。
    if (generation !== playbackGeneration || leaving) {
      await candidate.stop();
      return;
    }
    session = candidate;
    setOverlay('');
    video.play().catch(() => {
      document.getElementById('play-btn').hidden = false;
    });
  } catch (e) {
    if (generation === playbackGeneration && !leaving) setOverlay(`視聴を開始できませんでした: ${e.message}`);
  }
}

async function load() {
  if (leaving) return;
  const generation = ++playbackGeneration;
  const previous = session;
  session = null;
  if (previous) await previous.stop();
  let meta;
  try {
    meta = await api(`/api/streams/${encodeURIComponent(streamId)}`);
  } catch (e) {
    if (generation !== playbackGeneration || leaving) return;
    document.getElementById('title').textContent = '視聴できません';
    setOverlay(/ログイン/.test(e.message) ? '視聴にはログインが必要です' : e.message);
    return;
  }
  if (generation !== playbackGeneration || leaving) return;
  renderMeta(meta);
  if (meta.is_live) await startPlayback(generation);
  else setOverlay('配信は行われていません');
}

const playBtn = document.getElementById('play-btn');
playBtn.addEventListener('click', async () => {
  try {
    await document.getElementById('video').play();
    playBtn.hidden = true;
    setOverlay('');
  } catch { setOverlay('再生できませんでした。もう一度お試しください。'); }
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
});

volumeSlider.addEventListener('input', () => {
  const video = document.getElementById('video');
  video.volume = Number(volumeSlider.value) / 100;
  video.muted = video.volume === 0;
  updateMuteBtn();
});

document.getElementById('video').addEventListener('loadedmetadata', () => {
  const video = document.getElementById('video');
  muteBtn.hidden = false;
  volumeSlider.hidden = false;
  volumeSlider.value = Math.round(video.volume * 100);
  updateMuteBtn();
  fullscreenBtn.hidden = !(document.fullscreenEnabled || video.webkitEnterFullscreen);
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

connectEvents((event) => {
  if (event.type === 'resync') { load(); return; }
  if (leaving || event.stream_id !== streamId) return;
  if (event.type === 'stream_online') {
    load();
  } else if (event.type === 'stream_offline') {
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
