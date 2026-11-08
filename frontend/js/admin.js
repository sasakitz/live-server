// 管理者設定コンソール: ユーザ/配信/Webhook管理 + 概要

let users = [];
let streams = [];
let webhooks = [];

// ---------- タブ切り替え ----------
function setupTabs() {
  const tabs = Array.from(document.querySelectorAll('.tab'));
  const activate = (selected) => {
    tabs.forEach((tab) => {
      const active = tab === selected;
      tab.classList.toggle('is-active', active);
      tab.setAttribute('aria-selected', String(active));
      tab.tabIndex = active ? 0 : -1;
      document.getElementById(`tab-${tab.dataset.tab}`).classList.toggle('is-active', active);
    });
  };
  tabs.forEach((tab, index) => {
    tab.addEventListener('click', () => activate(tab));
    tab.addEventListener('keydown', (event) => {
      let next;
      if (event.key === 'ArrowRight') next = tabs[(index + 1) % tabs.length];
      else if (event.key === 'ArrowLeft') next = tabs[(index + tabs.length - 1) % tabs.length];
      else if (event.key === 'Home') next = tabs[0];
      else if (event.key === 'End') next = tabs[tabs.length - 1];
      if (next) { event.preventDefault(); activate(next); next.focus(); }
    });
  });
}

// ---------- 概要 ----------
async function loadOverview() {
  const stats = await api('/api/admin/overview');
  const el = document.getElementById('overview-stats');
  const items = [
    ['配信中', `${stats.active_streams}`],
    ['総視聴者数', `${stats.total_viewers}`],
    ['ユーザ数', `${stats.total_users}`],
    ['登録配信数', `${stats.total_streams}`],
  ];
  el.innerHTML = items.map(([label, value]) => `
    <div class="card stat-card">
      <p class="stat-card__label">${label}</p>
      <p class="stat-card__value">${value}</p>
    </div>
  `).join('');
}

// ---------- ユーザ管理 ----------
function renderUsers() {
  const tbody = document.getElementById('users-tbody');
  if (users.length === 0) {
    tbody.innerHTML = '<tr><td class="table-empty" colspan="5">ユーザはまだ登録されていません。</td></tr>';
    return;
  }
  tbody.innerHTML = users.map((u) => `
    <tr>
      <td>${escapeHtml(u.username)}</td>
      <td>${u.role === 'admin' ? '管理者' : '配信者'}</td>
      <td>${u.disabled ? '<span class="badge badge--offline">無効</span>' : '<span class="badge badge--public">有効</span>'}</td>
      <td>${new Date(u.created_at).toLocaleString('ja-JP')}</td>
      <td class="btn-row">
        <button class="btn btn--secondary btn--sm" data-action="edit-user" data-id="${u.id}">編集</button>
        <button class="btn btn--danger btn--sm" data-action="delete-user" data-id="${u.id}">削除</button>
      </td>
    </tr>
  `).join('');
}

function openUserForm(user) {
  document.getElementById('user-form-card').hidden = false;
  document.getElementById('user-form-title').textContent = user ? `ユーザ編集: ${user.username}` : '新規ユーザ';
  document.getElementById('user-id').value = user ? user.id : '';
  document.getElementById('user-username').value = user ? user.username : '';
  document.getElementById('user-username').disabled = Boolean(user);
  document.getElementById('user-password').value = '';
  document.getElementById('user-password-optional').hidden = !user;
  document.getElementById('user-password').required = !user;
  document.getElementById('user-role').value = user ? user.role : 'broadcaster';
  document.getElementById('user-disabled').checked = user ? user.disabled : false;
  document.getElementById(user ? 'user-password' : 'user-username').focus();
}

function closeUserForm() {
  document.getElementById('user-form-card').hidden = true;
  document.getElementById('user-form').reset();
}

async function loadUsers() {
  users = await api('/api/admin/users');
  renderUsers();
  const ownerSelect = document.getElementById('stream-owner');
  const selectedOwner = ownerSelect.value;
  ownerSelect.innerHTML = users.map((u) => `<option value="${u.id}">${escapeHtml(u.username)}</option>`).join('');
  if (users.some((user) => String(user.id) === selectedOwner)) ownerSelect.value = selectedOwner;
}

document.getElementById('user-new-btn').addEventListener('click', () => openUserForm(null));
document.getElementById('user-cancel-btn').addEventListener('click', closeUserForm);

document.getElementById('user-form').addEventListener('submit', async (ev) => {
  ev.preventDefault();
  const id = document.getElementById('user-id').value;
  const username = document.getElementById('user-username').value;
  const password = document.getElementById('user-password').value;
  const role = document.getElementById('user-role').value;
  const disabled = document.getElementById('user-disabled').checked;
  try {
    if (id) {
      const body = { role, disabled };
      if (password) body.password = password;
      await api(`/api/admin/users/${id}`, { method: 'PATCH', body });
    } else {
      await api('/api/admin/users', { method: 'POST', body: { username, password, role } });
    }
    closeUserForm();
    await loadUsers();
    showToast('保存しました');
  } catch (e) {
    showToast(e.message);
  }
});

document.getElementById('users-tbody').addEventListener('click', async (ev) => {
  const btn = ev.target.closest('button');
  if (!btn) return;
  const id = btn.dataset.id;
  if (btn.dataset.action === 'edit-user') {
    openUserForm(users.find((u) => String(u.id) === id));
  } else if (btn.dataset.action === 'delete-user') {
    if (!window.confirm('このユーザを削除します。よろしいですか?')) return;
    try {
      await api(`/api/admin/users/${id}`, { method: 'DELETE' });
      await loadUsers();
      showToast('削除しました');
    } catch (e) {
      showToast(e.message);
    }
  }
});

// ---------- 配信管理 ----------
function renderStreams() {
  const tbody = document.getElementById('streams-tbody');
  if (streams.length === 0) {
    tbody.innerHTML = '<tr><td class="table-empty" colspan="7">配信はまだ登録されていません。「＋ 新規配信」から作成してください。</td></tr>';
    return;
  }
  tbody.innerHTML = streams.map((s) => `
    <tr>
      <td><a href="/watch/${encodeURIComponent(s.id)}">${escapeHtml(s.id)}</a></td>
      <td>${escapeHtml(s.title)}</td>
      <td>${escapeHtml(s.owner_username)}</td>
      <td>${s.is_live ? '<span class="badge badge--live"><span class="badge__dot"></span>LIVE</span>' : '<span class="badge badge--offline">オフライン</span>'}</td>
      <td>${s.public ? '公開' : '限定'}</td>
      <td>${s.is_live ? s.viewer_count : '-'}</td>
      <td class="btn-row">
        <button class="btn btn--secondary btn--sm" data-action="edit-stream" data-id="${s.id}">編集</button>
        <button class="btn btn--secondary btn--sm" data-action="regen-stream" data-id="${s.id}" ${s.is_live ? 'disabled title="配信中は再発行できません"' : ''}>Key再発行</button>
        <button class="btn btn--danger btn--sm" data-action="stop-stream" data-id="${s.id}" ${s.is_live ? '' : 'disabled'}>強制停止</button>
        <button class="btn btn--danger btn--sm" data-action="delete-stream" data-id="${s.id}">削除</button>
      </td>
    </tr>
  `).join('');
}

function openStreamForm(stream) {
  document.getElementById('stream-form-card').hidden = false;
  document.getElementById('stream-form-title').textContent = stream ? `配信編集: ${stream.title}` : '新規配信';
  document.getElementById('stream-id').value = stream ? stream.id : '';
  document.getElementById('stream-id').disabled = Boolean(stream);
  document.getElementById('stream-owner').value = stream ? String(stream.owner_user_id) : '';
  document.getElementById('stream-title').value = stream ? stream.title : '';
  document.getElementById('stream-desc').value = stream ? stream.description : '';
  document.getElementById('stream-public').checked = stream ? stream.public : false;
  document.getElementById('stream-form').dataset.editingId = stream ? stream.id : '';
  document.getElementById('stream-title').focus();
}

function closeStreamForm() {
  document.getElementById('stream-form-card').hidden = true;
  document.getElementById('stream-form').reset();
  document.getElementById('stream-id').disabled = false;
}

async function loadStreams() {
  streams = await api('/api/admin/streams');
  renderStreams();
}

document.getElementById('stream-new-btn').addEventListener('click', () => openStreamForm(null));
document.getElementById('stream-cancel-btn').addEventListener('click', closeStreamForm);

document.getElementById('stream-form').addEventListener('submit', async (ev) => {
  ev.preventDefault();
  const editingId = ev.target.dataset.editingId;
  const title = document.getElementById('stream-title').value;
  const description = document.getElementById('stream-desc').value;
  const isPublic = document.getElementById('stream-public').checked;
  const ownerUserId = Number(document.getElementById('stream-owner').value);
  try {
    if (editingId) {
      await api(`/api/admin/streams/${encodeURIComponent(editingId)}`, {
        method: 'PATCH',
        body: { title, description, public: isPublic, owner_user_id: ownerUserId },
      });
    } else {
      const id = document.getElementById('stream-id').value;
      await api('/api/admin/streams', {
        method: 'POST',
        body: { id, owner_user_id: ownerUserId, title, description, public: isPublic },
      });
    }
    closeStreamForm();
    await loadStreams();
    showToast('保存しました');
  } catch (e) {
    showToast(e.message);
  }
});

document.getElementById('streams-tbody').addEventListener('click', async (ev) => {
  const btn = ev.target.closest('button');
  if (!btn || btn.disabled) return;
  const id = btn.dataset.id;
  const action = btn.dataset.action;
  try {
    if (action === 'edit-stream') {
      openStreamForm(streams.find((s) => s.id === id));
    } else if (action === 'regen-stream') {
      if (!window.confirm('Stream Keyを再発行します。よろしいですか?')) return;
      await api(`/api/admin/streams/${encodeURIComponent(id)}/regenerate-key`, { method: 'POST' });
      showToast('再発行しました');
      await loadStreams();
    } else if (action === 'stop-stream') {
      if (!window.confirm('配信を強制停止します。よろしいですか?')) return;
      await api(`/api/admin/streams/${encodeURIComponent(id)}/stop`, { method: 'POST' });
      showToast('停止しました');
      await loadStreams();
    } else if (action === 'delete-stream') {
      if (!window.confirm('この配信を削除します。よろしいですか?(元に戻せません)')) return;
      await api(`/api/admin/streams/${encodeURIComponent(id)}`, { method: 'DELETE' });
      showToast('削除しました');
      await loadStreams();
    }
  } catch (e) {
    showToast(e.message);
  }
});

// ---------- Webhook管理 ----------
function renderWebhooks() {
  const tbody = document.getElementById('webhooks-tbody');
  if (webhooks.length === 0) {
    tbody.innerHTML = '<tr><td class="table-empty" colspan="6">通知先は未設定です。DiscordなどのWebhookを登録できます。</td></tr>';
    return;
  }
  tbody.innerHTML = webhooks.map((w) => `
    <tr>
      <td>${escapeHtml(w.name)}</td>
      <td style="max-width:280px; overflow:hidden; text-overflow:ellipsis">${escapeHtml(w.url)}</td>
      <td>${w.notify_start ? '✓' : '-'}</td>
      <td>${w.notify_stop ? '✓' : '-'}</td>
      <td>${w.enabled ? '✓' : '-'}</td>
      <td class="btn-row">
        <button class="btn btn--secondary btn--sm" data-action="edit-webhook" data-id="${w.id}">編集</button>
        <button class="btn btn--danger btn--sm" data-action="delete-webhook" data-id="${w.id}">削除</button>
      </td>
    </tr>
  `).join('');
}

function openWebhookForm(webhook) {
  document.getElementById('webhook-form-card').hidden = false;
  document.getElementById('webhook-form-title').textContent = webhook ? `Webhook編集: ${webhook.name}` : '新規Webhook';
  document.getElementById('webhook-id').value = webhook ? webhook.id : '';
  document.getElementById('webhook-name').value = webhook ? webhook.name : '';
  document.getElementById('webhook-url').value = webhook ? webhook.url : '';
  document.getElementById('webhook-start').checked = webhook ? webhook.notify_start : true;
  document.getElementById('webhook-stop').checked = webhook ? webhook.notify_stop : true;
  document.getElementById('webhook-enabled').checked = webhook ? webhook.enabled : true;
  document.getElementById('webhook-name').focus();
}

function closeWebhookForm() {
  document.getElementById('webhook-form-card').hidden = true;
  document.getElementById('webhook-form').reset();
}

async function loadWebhooks() {
  webhooks = await api('/api/admin/webhooks');
  renderWebhooks();
}

document.getElementById('webhook-new-btn').addEventListener('click', () => openWebhookForm(null));
document.getElementById('webhook-cancel-btn').addEventListener('click', closeWebhookForm);

document.getElementById('webhook-test-btn').addEventListener('click', async () => {
  const url = document.getElementById('webhook-url').value;
  if (!url) {
    showToast('URLを入力してください');
    return;
  }
  try {
    await api('/api/admin/webhooks/test', { method: 'POST', body: { url } });
    showToast('テスト通知を送信しました');
  } catch (e) {
    showToast(e.message);
  }
});

document.getElementById('webhook-form').addEventListener('submit', async (ev) => {
  ev.preventDefault();
  const id = document.getElementById('webhook-id').value;
  const body = {
    name: document.getElementById('webhook-name').value,
    url: document.getElementById('webhook-url').value,
    notify_start: document.getElementById('webhook-start').checked,
    notify_stop: document.getElementById('webhook-stop').checked,
    enabled: document.getElementById('webhook-enabled').checked,
  };
  try {
    if (id) {
      await api(`/api/admin/webhooks/${id}`, { method: 'PATCH', body });
    } else {
      await api('/api/admin/webhooks', { method: 'POST', body });
    }
    closeWebhookForm();
    await loadWebhooks();
    showToast('保存しました');
  } catch (e) {
    showToast(e.message);
  }
});

document.getElementById('webhooks-tbody').addEventListener('click', async (ev) => {
  const btn = ev.target.closest('button');
  if (!btn) return;
  const id = btn.dataset.id;
  if (btn.dataset.action === 'edit-webhook') {
    openWebhookForm(webhooks.find((w) => String(w.id) === id));
  } else if (btn.dataset.action === 'delete-webhook') {
    if (!window.confirm('このWebhookを削除します。よろしいですか?')) return;
    try {
      await api(`/api/admin/webhooks/${id}`, { method: 'DELETE' });
      await loadWebhooks();
      showToast('削除しました');
    } catch (e) {
      showToast(e.message);
    }
  }
});

async function refreshAdmin() {
  const results = await Promise.allSettled([loadOverview(), loadUsers(), loadStreams(), loadWebhooks()]);
  const failed = results.find((result) => result.status === 'rejected');
  document.getElementById('admin-error').hidden = !failed;
  if (failed) document.getElementById('admin-error-message').textContent = `一部の情報を読み込めませんでした。${failed.reason.message}`;
}
document.getElementById('admin-retry').addEventListener('click', refreshAdmin);

// ---------- 起動 ----------
async function guardAdmin() {
  const user = await fetchMe();
  if (!user) {
    window.location.href = '/login?next=/admin';
    return false;
  }
  if (user.role !== 'admin') {
    window.location.href = '/dashboard';
    return false;
  }
  return true;
}

(async () => {
  if (!(await guardAdmin())) return;
  setupTabs();
  await refreshAdmin();

  connectEvents((event) => {
    if (event.type === 'resync' || event.type === 'stream_online' || event.type === 'stream_offline') {
      refreshAdmin();
    } else if (event.type === 'viewer_count') {
      const s = streams.find((x) => x.id === event.stream_id);
      if (s) {
        s.viewer_count = event.count;
        renderStreams();
      }
    }
  });
})();
