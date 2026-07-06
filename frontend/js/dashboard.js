// 配信管理ダッシュボード: 自分の配信一覧の表示・編集・stream_key管理

let uidCounter = 0;
let myStreams = [];

function streamCardEl(stream) {
  const tpl = document.getElementById('stream-template');
  const node = tpl.content.cloneNode(true);
  const root = document.createElement('div');
  root.appendChild(node);
  root.dataset.streamId = stream.id;

  uidCounter += 1;
  const checkboxId = `pub-${uidCounter}`;
  root.innerHTML = root.innerHTML.replace('pub-CHECKBOX_ID', checkboxId);
  root.querySelector('label[for^="pub-"]').setAttribute('for', checkboxId);
  root.querySelector('.js-public-input').id = checkboxId;

  fillCard(root, stream);
  wireCard(root, stream);
  return root;
}

function fillCard(root, stream) {
  root.querySelector('.js-title').textContent = stream.title;
  root.querySelector('.js-badge').innerHTML = stream.is_live
    ? '<span class="badge badge--live"><span class="badge__dot"></span>LIVE</span>'
    : '<span class="badge badge--offline">オフライン</span>';
  root.querySelector('.js-viewer').textContent = stream.is_live
    ? `👁 ${stream.viewer_count}人視聴中`
    : `これまでの最大同時視聴者数: ${stream.peak_viewers}人`;

  const watchUrl = `${window.location.origin}/watch/${encodeURIComponent(stream.id)}`;
  root.querySelector('.js-watch-url').textContent = watchUrl;

  const whipUrl = `${window.location.origin}/api/whip/${stream.stream_key}`;
  root.querySelector('.js-whip-url').textContent = whipUrl;
  root.querySelector('.js-key').textContent = stream.stream_key;
  root.querySelector('.js-key').dataset.revealed = 'false';
  root.querySelector('.js-key').textContent = maskKey(stream.stream_key);

  root.querySelector('.js-title-input').value = stream.title;
  root.querySelector('.js-desc-input').value = stream.description;
  root.querySelector('.js-public-input').checked = stream.public;

  root.querySelector('.js-stop-btn').hidden = !stream.is_live;
  root.querySelector('.js-regenerate-key').disabled = stream.is_live;
  root.querySelector('.js-regenerate-key').title = stream.is_live ? '配信中は再発行できません' : '';
}

function maskKey(key) {
  return '•'.repeat(Math.min(key.length, 32));
}

function wireCard(root, stream) {
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
  container.innerHTML = '';
  if (myStreams.length === 0) {
    empty.hidden = false;
    return;
  }
  empty.hidden = true;
  for (const stream of myStreams) {
    container.appendChild(streamCardEl(stream));
  }
}

async function load() {
  try {
    myStreams = await api('/api/my/streams');
  } catch (e) {
    showToast(e.message);
    myStreams = [];
  }
  renderAll();
}

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
    if (event.type === 'stream_online' || event.type === 'stream_offline') {
      load();
    } else if (event.type === 'viewer_count') {
      const stream = myStreams.find((s) => s.id === event.stream_id);
      if (stream) {
        stream.viewer_count = event.count;
        renderAll();
      }
    }
  });
})();
