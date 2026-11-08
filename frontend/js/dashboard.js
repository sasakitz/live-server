// 配信管理ダッシュボード: 自分の配信一覧の表示・編集・stream_key管理

let uidCounter = 0;
let myStreams = [];
let loadGeneration = 0;

function streamCardEl(stream) {
  const tpl = document.getElementById('stream-template');
  const node = tpl.content.cloneNode(true);
  const root = document.createElement('div');
  root.appendChild(node);
  root.dataset.streamId = stream.id;

  uidCounter += 1;
  root.querySelectorAll('[id], label[for]').forEach((el) => {
    if (el.id) el.id = el.id.replace('CHECKBOX_ID', uidCounter).replace('INPUT_ID', uidCounter);
    if (el.htmlFor) el.htmlFor = el.htmlFor.replace('CHECKBOX_ID', uidCounter).replace('INPUT_ID', uidCounter);
  });

  fillCard(root, stream);
  wireCard(root, stream);
  return root;
}

function updateCardStatus(root, stream) {
  root.querySelector('.js-title').textContent = stream.title;
  root.querySelector('.js-badge').innerHTML = stream.is_live
    ? '<span class="badge badge--live"><span class="badge__dot"></span>LIVE</span>'
    : '<span class="badge badge--offline">オフライン</span>';
  root.querySelector('.js-viewer').textContent = stream.is_live
    ? `${stream.viewer_count}人が視聴中`
    : `これまでの最大同時視聴者数: ${stream.peak_viewers}人`;

  root.querySelector('.js-stop-btn').hidden = !stream.is_live;
  root.querySelector('.js-regenerate-key').disabled = stream.is_live;
  root.querySelector('.js-regenerate-key').title = stream.is_live ? '配信中は再発行できません' : '';
}

function fillConnectionSettings(root, stream) {
  const watchUrl = `${window.location.origin}/watch/${encodeURIComponent(stream.id)}`;
  root.querySelector('.js-watch-url').textContent = watchUrl;
  root.querySelector('.js-watch-link').href = watchUrl;

  const whipUrl = `${window.location.origin}/api/whip/${stream.stream_key}`;
  root.querySelector('.js-whip-url').textContent = whipUrl;
  root.querySelector('.js-key').textContent = stream.stream_key;
  root.querySelector('.js-key').dataset.revealed = 'false';
  root.querySelector('.js-toggle-key').textContent = '表示';
  root.querySelector('.js-key').textContent = maskKey(stream.stream_key);

}

function fillCard(root, stream) {
  updateCardStatus(root, stream);
  fillConnectionSettings(root, stream);
  root.querySelector('.js-title-input').value = stream.title;
  root.querySelector('.js-desc-input').value = stream.description;
  root.querySelector('.js-public-input').checked = stream.public;

}

function maskKey(key) {
  return '•'.repeat(Math.min(key.length, 32));
}

function wireCard(root, stream) {
  const copy = async (value, message) => {
    try {
      await navigator.clipboard.writeText(value);
      showToast(message);
    } catch {
      showToast('コピーできませんでした。表示されたURLを選択してコピーしてください。');
    }
  };
  root.querySelector('.js-copy-watch').addEventListener('click', () => copy(root.querySelector('.js-watch-url').textContent, '視聴リンクをコピーしました'));
  root.querySelector('.js-copy-whip').addEventListener('click', () => copy(root.querySelector('.js-whip-url').textContent, 'OBS用サーバURLをコピーしました'));
  const keyEl = root.querySelector('.js-key');
  root.querySelector('.js-toggle-key').addEventListener('click', (ev) => {
    const revealed = keyEl.dataset.revealed === 'true';
    keyEl.textContent = revealed ? maskKey(stream.stream_key) : stream.stream_key;
    keyEl.dataset.revealed = String(!revealed);
    ev.target.textContent = revealed ? '表示' : '隠す';
  });

  root.querySelector('.js-copy-key').addEventListener('click', async () => {
    try {
      await navigator.clipboard.writeText(stream.stream_key);
      showToast('Stream Keyをコピーしました');
    } catch {
      showToast('コピーに失敗しました。手動で選択してください。');
    }
  });

  root.querySelector('.js-regenerate-key').addEventListener('click', async () => {
    if (!window.confirm('Stream Keyを再発行すると、現在の値は無効になります。よろしいですか?')) return;
    try {
      const updated = await api(`/api/my/streams/${encodeURIComponent(stream.id)}/regenerate-key`, { method: 'POST' });
      showToast('Stream Keyを再発行しました');
      Object.assign(stream, updated);
      fillCard(root, stream);
    } catch (e) {
      showToast(e.message);
    }
  });

  root.querySelector('.js-stop-btn').addEventListener('click', async () => {
    if (!window.confirm('配信を強制停止します。よろしいですか?')) return;
    try {
      const updated = await api(`/api/my/streams/${encodeURIComponent(stream.id)}/stop`, { method: 'POST' });
      Object.assign(stream, updated);
      fillCard(root, stream);
      showToast('配信を停止しました');
    } catch (e) {
      showToast(e.message);
    }
  });

  root.querySelector('.js-delete-btn').addEventListener('click', async () => {
    if (!window.confirm('この配信を削除します。よろしいですか?(元に戻せません)')) return;
    try {
      await api(`/api/my/streams/${encodeURIComponent(stream.id)}`, { method: 'DELETE' });
      showToast('配信を削除しました');
      await load();
    } catch (e) {
      showToast(e.message);
    }
  });

  root.querySelector('.js-edit-form').addEventListener('submit', async (ev) => {
    ev.preventDefault();
    const title = root.querySelector('.js-title-input').value;
    const description = root.querySelector('.js-desc-input').value;
    const isPublic = root.querySelector('.js-public-input').checked;
    try {
      const updated = await api(`/api/my/streams/${encodeURIComponent(stream.id)}`, {
        method: 'PATCH',
        body: { title, description, public: isPublic },
      });
      Object.assign(stream, updated);
      fillCard(root, stream);
      showToast('保存しました');
    } catch (e) {
      showToast(e.message);
    }
  });
}

function renderAll() {
  const container = document.getElementById('streams');
  const empty = document.getElementById('empty');
  const existing = new Map(Array.from(container.children).map((node) => [node.dataset.streamId, node]));
  if (myStreams.length === 0) {
    empty.hidden = false;
    container.replaceChildren();
    return;
  }
  empty.hidden = true;
  const nodes = myStreams.map((stream) => {
    const root = existing.get(stream.id);
    if (root) { updateCardStatus(root, stream); fillConnectionSettings(root, stream); return root; }
    return streamCardEl(stream);
  });
  const keep = new Set(nodes);
  Array.from(container.children).forEach((node) => { if (!keep.has(node)) node.remove(); });
  nodes.forEach((node, index) => {
    if (container.children[index] !== node) container.insertBefore(node, container.children[index] || null);
  });
}

async function load() {
  const generation = ++loadGeneration;
  try {
    const result = await api('/api/my/streams');
    if (generation !== loadGeneration) return;
    // イベント更新で入力中のフォームやボタンのクロージャを破棄しない。
    const existing = new Map(myStreams.map((stream) => [stream.id, stream]));
    myStreams = result.map((stream) => existing.has(stream.id) ? Object.assign(existing.get(stream.id), stream) : stream);
  } catch (e) {
    if (generation !== loadGeneration) return;
    document.getElementById('dashboard-error-message').textContent = `配信を読み込めませんでした。${e.message}`;
    document.getElementById('dashboard-error').hidden = false;
    document.getElementById('dashboard-loading').hidden = true;
    document.getElementById('empty').hidden = true;
    return;
  }
  document.getElementById('dashboard-error').hidden = true;
  document.getElementById('dashboard-loading').hidden = true;
  renderAll();
}

function closeStreamForm() {
  document.getElementById('stream-form-card').hidden = true;
  document.getElementById('stream-form').reset();
}

document.getElementById('stream-new-btn').addEventListener('click', () => {
  document.getElementById('stream-form-card').hidden = false;
  document.getElementById('stream-id').focus();
});
document.getElementById('stream-cancel-btn').addEventListener('click', closeStreamForm);
document.getElementById('dashboard-retry').addEventListener('click', load);

document.getElementById('stream-form').addEventListener('submit', async (ev) => {
  ev.preventDefault();
  const id = document.getElementById('stream-id').value;
  const title = document.getElementById('stream-title').value;
  const description = document.getElementById('stream-desc').value;
  const isPublic = document.getElementById('stream-public').checked;
  try {
    await api('/api/my/streams', { method: 'POST', body: { id, title, description, public: isPublic } });
    closeStreamForm();
    await load();
    showToast('配信を作成しました');
  } catch (e) {
    showToast(e.message);
  }
});

async function guardLoggedIn() {
  const user = await fetchMe();
  if (!user) {
    window.location.href = '/login?next=/dashboard';
    return false;
  }
  return true;
}

(async () => {
  if (!(await guardLoggedIn())) return;
  await load();
  connectEvents((event) => {
    if (event.type === 'resync' || event.type === 'stream_online' || event.type === 'stream_offline') {
      load();
    } else if (event.type === 'viewer_count') {
      const stream = myStreams.find((s) => s.id === event.stream_id);
      if (stream) {
        stream.viewer_count = event.count;
        const root = Array.from(document.getElementById('streams').children).find((el) => el.dataset.streamId === stream.id);
        if (root && stream.is_live) root.querySelector('.js-viewer').textContent = `${stream.viewer_count}人が視聴中`;
      }
    }
  });
})();
