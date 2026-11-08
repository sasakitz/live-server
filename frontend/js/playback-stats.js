// getStatsの未対応項目は「—」とし、取得できた値だけを表示する。
async function readPlaybackStats(pc, previous = new Map()) {
  const reports = await pc.getStats();
  const samples = new Map();
  const lines = [`接続: ${pc.connectionState} / ICE: ${pc.iceConnectionState || '—'}`];
  const number = (value, digits = 0) => Number.isFinite(value) ? value.toFixed(digits) : '—';
  for (const report of reports.values()) {
    if (report.type !== 'inbound-rtp' || report.isRemote) continue;
    const kind = report.kind || report.mediaType;
    if (kind !== 'video' && kind !== 'audio') continue;
    samples.set(report.id, report);
    const before = previous.get(report.id);
    const seconds = before ? (report.timestamp - before.timestamp) / 1000 : 0;
    const delta = (field) => seconds > 0 && Number.isFinite(report[field])
      && Number.isFinite(before[field]) && report[field] >= before[field]
      ? report[field] - before[field] : NaN;
    const codec = reports.get(report.codecId);
    lines.push('', kind === 'video' ? '映像' : '音声');
    if (codec) lines.push(`コーデック: ${codec.mimeType || '—'} ${codec.sdpFmtpLine || ''}`.trim());
    lines.push(`受信: ${number(delta('bytesReceived') * 8 / seconds / 1000)} kbps`);
    lines.push(`パケット欠落: ${number(report.packetsLost)}（累計） / 揺らぎ: ${number(report.jitter * 1000, 1)} ms`);
    lines.push(`再送要求: ${number(report.nackCount)} / キーフレーム要求: ${number(report.pliCount)}（累計）`);
    const emitted = delta('jitterBufferEmittedCount');
    lines.push(`受信バッファ待ち: ${number(emitted > 0 ? delta('jitterBufferDelay') * 1000 / emitted : NaN, 1)} ms`);
    if (kind === 'video') {
      lines.push(`解像度: ${number(report.frameWidth)} × ${number(report.frameHeight)}`);
      lines.push(`復号: ${number(delta('framesDecoded') / seconds, 1)} fps / 破棄: ${number(report.framesDropped)}フレーム（累計）`);
      lines.push(`映像停止: ${number(report.freezeCount)}回 / ${number(report.totalFreezesDuration, 1)}秒（累計）`);
      if (report.decoderImplementation) lines.push(`デコーダ: ${report.decoderImplementation}`);
    } else {
      lines.push(`音声補完: ${number(delta('concealedSamples'))}サンプル（前回表示以降）`);
    }
  }
  // 選択済みの経路だけを表示する。IPアドレスやセッションURLは含めない。
  const transport = [...reports.values()].find((report) => report.type === 'transport' && report.selectedCandidatePairId);
  const pair = transport ? reports.get(transport.selectedCandidatePairId)
    : [...reports.values()].find((report) => report.type === 'candidate-pair' && report.nominated && report.state === 'succeeded');
  if (pair) {
    const remote = reports.get(pair.remoteCandidateId);
    lines.push('', `往復遅延: ${number(pair.currentRoundTripTime * 1000, 1)} ms`);
    if (remote) lines.push(`通信方式: ${remote.protocol || '—'} / ${remote.candidateType || '—'}`);
  }
  if (!samples.size) lines.push('', '受信情報を待っています。');
  return { text: lines.join('\n'), samples };
}
