// WHEP(WebRTC-HTTP Egress Protocol)クライアント。videoタグへライブ映像を流し込む。

/**
 * ICE gathering完了を待つ(trickle ICEを使わないシンプルな実装)。
 * @param {RTCPeerConnection} pc
 */
function waitIceGatheringComplete(pc) {
  if (pc.iceGatheringState === 'complete') return Promise.resolve();
  return new Promise((resolve) => {
    function finish() {
      clearTimeout(timer);
      pc.removeEventListener('icegatheringstatechange', check);
      resolve();
    }
    function check() {
      if (pc.iceGatheringState === 'complete') finish();
    }
    pc.addEventListener('icegatheringstatechange', check);
    // ICE候補収集が長引く場合でも視聴開始できるよう、タイムアウトで打ち切る。
    const timer = setTimeout(finish, 4000);
  });
}

/**
 * WHEPエンドポイントに接続し、videoElへ映像/音声を流し込む。
 * @param {string} streamId
 * @param {HTMLVideoElement} videoEl
 * @param {(state: RTCPeerConnectionState) => void} [onStateChange]
 * @param {() => boolean} [isCurrent] この接続が現在の視聴操作に属するか。
 * @returns {Promise<{ pc: RTCPeerConnection, stop: () => Promise<void> }>}
 */
async function startWhep(streamId, videoEl, onStateChange, isCurrent = () => true) {
  let stunServers = [];
  try {
    const cfg = await api('/api/client-config');
    stunServers = cfg.stun_servers || [];
  } catch {
    // 取得できなくても(サーバがpublic_ipに直接到達可能なため)致命的ではない。
  }

  const pc = new RTCPeerConnection({
    iceServers: stunServers.map((url) => ({ urls: url })),
  });
  // streamsが空、または音声と映像が別streamで届いても同じvideoで再生する。
  const media = new MediaStream();

  let resourceUrl;
  let stopped = false;
  const stop = async () => {
    if (stopped) return;
    stopped = true;
    pc.close();
    if (videoEl.srcObject === media) videoEl.srcObject = null;
    if (resourceUrl) {
      try {
        await fetch(resourceUrl, { method: 'DELETE', credentials: 'same-origin', keepalive: true });
      } catch {
        // ページ離脱時などは失敗しても無視(サーバ側のICE切断検知に任せる)。
      }
    }
  };

  try {
    pc.addEventListener('track', (event) => {
      if (stopped || !isCurrent()) return;
      if (!media.getTracks().includes(event.track)) media.addTrack(event.track);
      if (videoEl.srcObject !== media) videoEl.srcObject = media;
    });
    if (onStateChange) {
      pc.addEventListener('connectionstatechange', () => onStateChange(pc.connectionState));
    }

    pc.addTransceiver('video', { direction: 'recvonly' });
    pc.addTransceiver('audio', { direction: 'recvonly' });

    const offer = await pc.createOffer();
    await pc.setLocalDescription(offer);
    await waitIceGatheringComplete(pc);

    const res = await fetch(`/api/whep/${encodeURIComponent(streamId)}`, {
      method: 'POST',
      credentials: 'same-origin',
      headers: { 'Content-Type': 'application/sdp' },
      body: pc.localDescription.sdp,
    });
    if (!res.ok) {
      const text = await res.text().catch(() => '');
      throw new Error(text || `視聴セッションの開始に失敗しました (${res.status})`);
    }
    resourceUrl = res.headers.get('Location');
    const answerSdp = await res.text();
    await pc.setRemoteDescription({ type: 'answer', sdp: answerSdp });
  } catch (error) {
    await stop();
    throw error;
  }

  return { pc, stop };
}
