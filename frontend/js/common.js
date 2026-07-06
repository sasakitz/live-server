// live-server: 全ページ共通のユーティリティ (ビルド不要のバニラJS)

/**
 * fetch()の薄いラッパー。JSON送受信・Cookie送信・エラー時のメッセージ抽出を行う。
 * @param {string} path
 * @param {RequestInit} [opts]
 */
async function api(path, opts = {}) {
  const headers = Object.assign({}, opts.headers);
  let body = opts.body;
  if (body !== undefined && !(body instanceof FormData)) {
    headers['Content-Type'] = 'application/json';
    body = JSON.stringify(body);
  }
  const res = await fetch(path, {
    credentials: 'same-origin',
    ...opts,
    headers,
    body,
  });
  if (res.status === 204) {
    return null;
  }
  const isJson = (res.headers.get('content-type') || '').includes('application/json');
  const data = isJson ? await res.json().catch(() => null) : await res.text();
  if (!res.ok) {
    const message = (data && typeof data === 'object' && data.error) || `リクエストに失敗しました (${res.status})`;
    throw new Error(message);
  }
  return data;
}

function escapeHtml(str) {
  return String(str ?? '').replace(/[&<>"']/g, (c) => ({
    '&': '&amp;',
    '<': '&lt;',
    '>': '&gt;',
    '"': '&quot;',
    "'": '&#39;',
  }[c]));
}

let toastTimer = null;
function showToast(message) {
  let el = document.getElementById('toast');
  if (!el) {
    el = document.createElement('div');
    el.id = 'toast';
    el.className = 'toast';
    document.body.appendChild(el);
  }
  el.textContent = message;
  el.classList.add('is-visible');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.classList.remove('is-visible'), 3200);
}

function formatRelativeStart(startedAt) {
  if (!startedAt) return '';
  const start = new Date(startedAt).getTime();
  const diffSec = Math.max(0, Math.floor((Date.now() - start) / 1000));
  const h = Math.floor(diffSec / 3600);
  const m = Math.floor((diffSec % 3600) / 60);
  const s = diffSec % 60;
  if (h > 0) return `${h}時間${m}分経過`;
  if (m > 0) return `${m}分${s}秒経過`;
  return `${s}秒経過`;
}

/** 現在のログインユーザ情報を取得する。未ログインならnull。 */
async function fetchMe() {
  try {
    return await api('/api/me');
  } catch {
    return null;
  }
}

/** 共通ヘッダーの右側ナビゲーションを、ログイン状態に応じて描画する。 */
async function renderNav() {
  const nav = document.getElementById('nav-links');
  if (!nav) return null;
  const user = await fetchMe();

  const links = [];
  if (user) {
    links.push(`<span class="text-muted">${escapeHtml(user.username)}</span>`);
    links.push('<a href="/dashboard" class="btn btn--secondary btn--sm">配信管理</a>');
    if (user.role === 'admin') {
      links.push('<a href="/admin" class="btn btn--secondary btn--sm">管理者設定</a>');
    }
    links.push('<button id="logout-btn" class="btn btn--secondary btn--sm" type="button">ログアウト</button>');
  } else {
    links.push('<a href="/login" class="btn btn--sm">ログイン</a>');
  }
  nav.innerHTML = links.join('');

  const logoutBtn = document.getElementById('logout-btn');
  if (logoutBtn) {
    logoutBtn.addEventListener('click', async () => {
      await api('/api/logout', { method: 'POST' });
      window.location.href = '/';
    });
  }
  return user;
}

/**
 * `/api/ws` に接続し、切断時は自動で再接続する(指数バックオフ、上限10秒)。
 * @param {(event: {type: string, [key: string]: any}) => void} onEvent
 */
function connectEvents(onEvent) {
  let delay = 1000;
  let closedByUser = false;

  function connect() {
    const proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    const ws = new WebSocket(`${proto}//${window.location.host}/api/ws`);
    ws.addEventListener('open', () => {
      delay = 1000;
    });
    ws.addEventListener('message', (ev) => {
      try {
        onEvent(JSON.parse(ev.data));
      } catch {
        // 無視: 不正なメッセージ
      }
    });
    ws.addEventListener('close', () => {
      if (closedByUser) return;
      setTimeout(connect, delay);
      delay = Math.min(delay * 1.6, 10000);
    });
    ws.addEventListener('error', () => ws.close());
    return ws;
  }

  const ws = connect();
  return () => {
    closedByUser = true;
    ws.close();
  };
}

document.addEventListener('DOMContentLoaded', () => {
  renderNav();
});
