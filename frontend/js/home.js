// Live discovery: distinguish loading, offline, search results, and request errors.
let streams = [];
let loadFailed = false;
let loadGeneration = 0;
const searchInput = document.getElementById('stream-search');

function render() {
  const grid = document.getElementById('live-grid');
  const query = searchInput.value.trim().toLocaleLowerCase('ja');
  const filtered = streams.filter((s) => `${s.title} ${s.description}`.toLocaleLowerCase('ja').includes(query));
  document.getElementById('hero-count').textContent = loadFailed ? '—' : streams.length;
  document.getElementById('hero-status').textContent = loadFailed ? '取得できません' : streams.length > 0 ? 'LIVE NOW' : 'STANDBY';
  document.getElementById('live-count').textContent = loadFailed ? '取得できません' : `${filtered.length}件`;
  document.getElementById('empty').hidden = loadFailed || filtered.length > 0;
  document.getElementById('empty-title').textContent = query ? '条件に合うライブが見つかりません' : '現在配信中のライブはありません';
  document.getElementById('empty-description').textContent = query
    ? 'タイトルや説明に一致する配信はありません。' : '配信開始時に一覧を自動更新します。';
  document.getElementById('clear-search').hidden = !query;
  grid.innerHTML = filtered.map((s) => `
    <a class="card stream-card" href="/watch/${encodeURIComponent(s.id)}">
      <div class="stream-card__thumb">
        <span class="stream-card__placeholder" aria-hidden="true">
          <svg viewBox="0 0 48 48" fill="none" stroke="currentColor" stroke-width="1.5"><rect x="4" y="8" width="40" height="29" rx="7"/><path d="m20 17 11 6-11 6zM18 43h12M24 37v6"/></svg>
          μTube LIVE
        </span>
        <img class="stream-card__thumb-img" data-stream-id="${escapeHtml(s.id)}"
          src="/api/streams/${encodeURIComponent(s.id)}/thumbnail.jpg" alt="" loading="lazy"
          onerror="this.hidden = true" onload="this.hidden = false" />
        <span class="badge badge--live"><span class="badge__dot"></span>LIVE</span>
      </div>
      <div class="stream-card__body">
        <h3 class="stream-card__title">${escapeHtml(s.title)}</h3>
        ${s.description ? `<p class="stream-card__description">${escapeHtml(s.description)}</p>` : ''}
        <p class="stream-card__meta"><span>${Number(s.viewer_count)}人が視聴中</span><span>${s.public ? '公開配信' : '視聴する ↗'}</span></p>
      </div>
    </a>
  `).join('');
}

function refreshThumbnails() {
  document.querySelectorAll('.stream-card__thumb-img').forEach((img) => {
    img.src = `/api/streams/${encodeURIComponent(img.dataset.streamId)}/thumbnail.jpg?t=${Date.now()}`;
  });
}

async function load() {
  const generation = ++loadGeneration;
  const error = document.getElementById('load-error');
  const retry = document.getElementById('retry-btn');
  retry.disabled = true;
  document.getElementById('live-grid').setAttribute('aria-busy', 'true');
  try {
    const result = await api('/api/streams');
    if (generation !== loadGeneration) return;
    streams = result;
    loadFailed = false;
    error.hidden = true;
  } catch (e) {
    if (generation !== loadGeneration) return;
    loadFailed = true;
    streams = [];
    document.getElementById('load-error-message').textContent = `ライブ一覧を読み込めませんでした。${e.message}`;
    error.hidden = false;
  } finally {
    if (generation !== loadGeneration) return;
    retry.disabled = false;
    document.getElementById('loading').hidden = true;
    document.getElementById('live-grid').setAttribute('aria-busy', 'false');
    render();
  }
}

searchInput.addEventListener('input', render);
document.getElementById('retry-btn').addEventListener('click', load);
document.getElementById('clear-search').addEventListener('click', () => {
  searchInput.value = '';
  render();
  searchInput.focus();
});
load();
setInterval(refreshThumbnails, 10000);
connectEvents((event) => {
  if (event.type === 'resync' || event.type === 'stream_online' || event.type === 'stream_offline') load();
  else if (event.type === 'viewer_count') {
    const stream = streams.find((s) => s.id === event.stream_id);
    if (stream) { stream.viewer_count = event.count; render(); }
  }
});
