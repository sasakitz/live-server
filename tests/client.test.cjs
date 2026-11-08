const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { test } = require('node:test');
const vm = require('node:vm');
const path = require('node:path');

function loadScript(file, extra = {}) {
  const timers = new Map();
  let nextTimer = 0;
  const context = vm.createContext({
    document: { addEventListener() {} },
    window: { location: { protocol: 'https:', host: 'localhost' } },
    setTimeout(fn) { timers.set(++nextTimer, fn); return nextTimer; },
    clearTimeout(id) { timers.delete(id); },
    ...extra,
  });
  vm.runInContext(readFileSync(path.join(__dirname, '../frontend/js', file), 'utf8'), context);
  return { context, timers };
}

class FakeSocket extends EventTarget {
  static instances = [];
  constructor() { super(); this.closed = false; FakeSocket.instances.push(this); }
  close() { this.closed = true; this.dispatchEvent(new Event('close')); }
}

for (const reconnect of [false, true]) {
  test(`WebSocket cleanup cancels reconnect and closes current socket (reconnected=${reconnect})`, () => {
    FakeSocket.instances = [];
    const { context, timers } = loadScript('common.js', { WebSocket: FakeSocket });
    const stop = context.connectEvents(() => {});
    FakeSocket.instances[0].close();
    assert.equal(timers.size, 1);
    const callback = [...timers.values()][0];
    if (reconnect) {
      timers.clear();
      callback();
      assert.equal(FakeSocket.instances.length, 2);
    }
    stop();
    assert.equal(timers.size, 0);
    assert.equal(FakeSocket.instances.at(-1).closed, true);
    callback(); // A queued callback must also respect explicit shutdown.
    assert.equal(FakeSocket.instances.length, reconnect ? 2 : 1);
  });
}

class FakePeer extends EventTarget {
  static instances = [];
  static failAt;
  constructor() {
    super();
    this.iceGatheringState = 'complete';
    this.localDescription = { sdp: 'offer' };
    this.closeCount = 0;
    FakePeer.instances.push(this);
  }
  addTransceiver() {}
  async createOffer() {
    if (FakePeer.failAt === 'offer') throw new Error('offer failed');
    return this.localDescription;
  }
  async setLocalDescription() {}
  async setRemoteDescription() {
    if (FakePeer.failAt === 'answer') throw new Error('answer failed');
  }
  close() { this.closeCount++; }
}

function whepHarness(failAt) {
  FakePeer.instances = [];
  FakePeer.failAt = failAt;
  const requests = [];
  const harness = loadScript('whep.js', {
    RTCPeerConnection: FakePeer,
    api: async () => ({ stun_servers: [] }),
    fetch: async (url, options) => {
      requests.push({ url, options });
      if (options.method === 'POST' && failAt === 'network') throw new Error('network failed');
      return {
        ok: failAt !== 'http', status: 403,
        headers: { get: () => '/api/whep/resource/test' },
        async text() {
          if (failAt === 'body') throw new Error('body failed');
          return 'answer';
        },
      };
    },
  });
  return { ...harness, requests };
}

for (const failAt of ['offer', 'network', 'http', 'body', 'answer']) {
  test(`WHEP cleans up when ${failAt} fails`, async () => {
    const { context, requests } = whepHarness(failAt);
    await assert.rejects(context.startWhep('test', {}));
    assert.equal(FakePeer.instances[0].closeCount, 1);
    const deletes = requests.filter(({ options }) => options.method === 'DELETE');
    assert.equal(deletes.length, ['body', 'answer'].includes(failAt) ? 1 : 0);
  });
}

test('WHEP stop is idempotent', async () => {
  const { context, requests } = whepHarness();
  const session = await context.startWhep('test', {});
  await session.stop();
  await session.stop();
  assert.equal(session.pc.closeCount, 1);
  assert.equal(requests.filter(({ options }) => options.method === 'DELETE').length, 1);
});

for (const complete of [false, true]) {
  test(`ICE wait releases timer and listener (complete=${complete})`, async () => {
    const { context, timers } = whepHarness();
    const pc = new FakePeer();
    pc.iceGatheringState = 'gathering';
    let removals = 0;
    pc.removeEventListener = () => { removals++; };
    const waiting = context.waitIceGatheringComplete(pc);
    if (complete) {
      pc.iceGatheringState = 'complete';
      pc.dispatchEvent(new Event('icegatheringstatechange'));
    } else {
      [...timers.values()][0]();
    }
    await waiting;
    assert.equal(timers.size, 0);
    assert.equal(removals, 1);
  });
}

function homeHarness(apiImpl) {
  const elements = new Map();
  const getElement = (id) => {
    if (!elements.has(id)) elements.set(id, {
      value: '', textContent: '', innerHTML: '', hidden: false,
      setAttribute() {}, addEventListener() {}, focus() {},
    });
    return elements.get(id);
  };
  let eventHandler;
  const harness = loadScript('home.js', {
    document: { getElementById: getElement, querySelectorAll: () => [] },
    api: apiImpl,
    escapeHtml: (value) => String(value).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c])),
    setInterval() {},
    connectEvents(handler) { eventHandler = handler; },
  });
  return { ...harness, getElement, event: (event) => eventHandler(event) };
}

test('Live search matches descriptions and safely renders stream titles', async () => {
  const { context, getElement } = homeHarness(async () => [
    { id: 'music', title: '<音楽ライブ>', description: 'ピアノの時間', viewer_count: 3, public: true },
    { id: 'talk', title: '雑談', description: '今日のこと', viewer_count: 1, public: true },
  ]);
  await new Promise(setImmediate);
  assert.equal(getElement('hero-count').textContent, 2);
  assert.match(getElement('live-grid').innerHTML, /&lt;音楽ライブ&gt;/);
  getElement('stream-search').value = ' ピアノ ';
  context.render();
  assert.equal(getElement('live-count').textContent, '1件');
  assert.doesNotMatch(getElement('live-grid').innerHTML, /watch\/talk/);
  assert.equal(getElement('empty').hidden, true);
});

test('A failed live request shows an error instead of an offline message and can recover', async () => {
  let fail = true;
  const { context, getElement } = homeHarness(async () => {
    if (fail) throw new Error('接続エラー');
    return [];
  });
  await new Promise(setImmediate);
  assert.equal(getElement('load-error').hidden, false);
  assert.equal(getElement('empty').hidden, true);
  assert.equal(getElement('hero-count').textContent, '—');
  fail = false;
  await context.load();
  assert.equal(getElement('load-error').hidden, true);
  assert.equal(getElement('empty').hidden, false);
  assert.equal(getElement('hero-count').textContent, 0);
});

test('Viewer updates preserve the active live search', async () => {
  const { context, getElement, event } = homeHarness(async () => [
    { id: 'music', title: 'Music', description: '', viewer_count: 1, public: true },
  ]);
  await new Promise(setImmediate);
  getElement('stream-search').value = 'absent';
  context.render();
  event({ type: 'viewer_count', stream_id: 'music', count: 4 });
  assert.equal(getElement('live-count').textContent, '0件');
  assert.equal(getElement('empty').hidden, false);
  assert.equal(getElement('clear-search').hidden, false);
});

function watchHarness() {
  class Element extends EventTarget {
    constructor() { super(); this.hidden = false; this.textContent = ''; this.value = '100'; this.volume = 1; this.muted = true; this.classList = { add() {}, remove() {} }; }
    setAttribute() {}
    async play() {}
  }
  const elements = new Map();
  const getElement = (id) => {
    if (!elements.has(id)) elements.set(id, new Element());
    return elements.get(id);
  };
  const window = new EventTarget();
  window.location = { pathname: '/watch/test' };
  let onEvent;
  const pending = [];
  const { context } = loadScript('watch.js', {
    window,
    document: { getElementById: getElement, querySelector: () => getElement('controls') },
    api: async () => ({ title: 'Test', is_live: true, started_at: null, viewer_count: 1 }),
    startWhep: () => new Promise((resolve) => pending.push(resolve)),
    setInterval() {}, clearInterval() {},
    connectEvents(handler) { onEvent = handler; },
  });
  return { context, getElement, window, pending, event: (ev) => onEvent(ev) };
}

for (const stopReason of ['offline', 'pagehide']) {
  test(`A late WHEP response after ${stopReason} is closed instead of kept alive`, async () => {
    const harness = watchHarness();
    await new Promise(setImmediate);
    if (stopReason === 'offline') harness.event({ type: 'stream_offline', stream_id: 'test' });
    else harness.window.dispatchEvent(new Event('pagehide'));
    let stopped = 0;
    harness.pending[0]({ stop: async () => { stopped++; } });
    await new Promise(setImmediate);
    assert.equal(stopped, 1);
    if (stopReason === 'offline') assert.equal(harness.getElement('overlay').textContent, '配信は終了しました');
  });
}

test('Overlapping watch loads keep only the newest connection', async () => {
  const harness = watchHarness();
  await new Promise(setImmediate);
  const latestLoad = harness.context.load();
  await new Promise(setImmediate);
  let oldStopped = 0;
  let newStopped = 0;
  harness.pending[1]({ stop: async () => { newStopped++; } });
  await latestLoad;
  harness.pending[0]({ stop: async () => { oldStopped++; } });
  await new Promise(setImmediate);
  assert.equal(oldStopped, 1);
  assert.equal(newStopped, 0);
  harness.window.dispatchEvent(new Event('pagehide'));
  assert.equal(newStopped, 1);
});

test('A stale list request cannot replace newer live results', async () => {
  const pending = [];
  const { context, getElement } = homeHarness(() => new Promise((resolve) => pending.push(resolve)));
  const newer = context.load();
  pending[1]([{ id: 'new', title: 'New live', description: '', viewer_count: 1, public: true }]);
  await newer;
  pending[0]([]);
  await new Promise(setImmediate);
  assert.equal(getElement('hero-count').textContent, 1);
  assert.match(getElement('live-grid').innerHTML, /New live/);
});

test('Dashboard live updates keep unsaved edits while refreshing rotated connection keys', async () => {
  const fields = new Map();
  const field = (name) => {
    if (!fields.has(name)) fields.set(name, { value: '', textContent: '', dataset: {}, addEventListener() {} });
    return fields.get(name);
  };
  const root = { dataset: { streamId: 'test' }, querySelector: field };
  const container = { children: [root], replaceChildren() { throw new Error('must not detach an edited card'); } };
  const element = (id) => id === 'streams' ? container : field(id);
  const original = { id: 'test', title: 'Original', description: 'description', stream_key: 'old', is_live: false, peak_viewers: 0 };
  const { context } = loadScript('dashboard.js', {
    document: { getElementById: element },
    window: { location: { origin: 'https://local.test' } },
    fetchMe: async () => null,
    api: async () => [{ ...original, stream_key: 'new', is_live: true, viewer_count: 3 }],
    fixture: original,
  });
  vm.runInContext('myStreams = [fixture]', context);
  field('.js-title-input').value = 'Unsaved draft';
  await context.load();
  assert.equal(field('.js-title-input').value, 'Unsaved draft');
  assert.equal(field('.js-whip-url').textContent, 'https://local.test/api/whip/new');
  assert.equal(field('.js-viewer').textContent, '3人が視聴中');
});
