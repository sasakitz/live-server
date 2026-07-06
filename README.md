# live-server

OBSからの配信をWHIP(WebRTC-HTTP Ingestion Protocol)で受け付け、ブラウザへWHEP
(WebRTC-HTTP Egress Protocol)で低遅延配信するRust製ライブ配信サーバです。
トランスコードを行わずRTPをそのまま中継するため、**遅延は概ね0.3〜0.7秒**です。

## 特徴

- OBS 30以降のWHIP出力に対応(H.264 + Opus固定、iOS Safariを含むモバイル対応)
- 10名程度の同時多元配信に対応するSFU構成
- ユーザ管理・配信管理の管理画面(バニラJS/CSS、ビルド不要)
- 配信開始/終了をDiscord(互換Webhook)へ通知
- スマートフォン対応のレスポンシブな視聴/管理画面
- Ubuntu Server 24.04でsystemdサービスとして常駐可能

## 構成

```
OBS(WHIP) → [live-server: 単一Rustバイナリ] → ブラウザ(WHEP)
             ├─ Axum: REST API / WebSocket / 静的ファイル配信
             ├─ webrtc-rs: SFUコア(WHIP受信・WHEPファンアウト)
             └─ SQLite: ユーザ/配信/Webhook設定の永続化
```

署名交換(WHIP/WHEPのHTTPリクエスト)はnginx/Caddy等のリバースプロキシ経由でTLS終端しますが、
メディア本体(音声/映像)はUDPでこのサーバへ直接到達します。

## ローカルでの動作確認

```bash
cp config.example.toml config.toml
# config.toml の [webrtc] public_ips はLAN内であれば手元PCのLAN IPのままで動作確認可能
cargo run --release -- --config config.toml
```

起動すると `http://127.0.0.1:8080` で待ち受けます。`config.toml` の
`auth.admin_bootstrap` に設定したユーザ名/パスワードで `/login` からログインできます。

OBSでの配信テスト:

1. OBSの「設定 → 配信」で サービス: `WHIP(カスタム)` を選択
2. サーバ: ダッシュボード(`/dashboard`)に表示される「サーバ」の値をコピー
   (例: `http://127.0.0.1:8080/api/whip/<stream_key>`)
3. Bearer Token: 空欄のまま(OBSのバージョンによっては接続できない場合があるので、
   その際はStream Key(ダッシュボードに表示)を入力する)
4. エンコーダ: H.264(x264 / NVENC / QuickSync)、音声はOpus
5. 配信開始 → `/watch/<配信ID>` で視聴

## 本番デプロイ (Ubuntu Server 24.04)

前提: Rustツールチェイン(`cargo`)がビルドマシンに入っていること。

```bash
cargo build --release
sudo ./deploy/install.sh
```

`install.sh` は以下を行います:

- 専用ユーザ `live-server` (ログイン不可)の作成
- `/var/lib/live-server` (データ) と `/etc/live-server` (設定) の作成
- バイナリを `/usr/local/bin/live-server` に配置
- 設定例を `/etc/live-server/config.toml` に配置(既存があれば上書きしない)
- systemdユニット `live-server.service` の配置

インストール後、スクリプトの指示に従って:

1. `/etc/live-server/config.toml` を編集
   - `webrtc.public_ips`: このサーバの外部公開IPアドレス(必須。ICE候補の広告に使う。
     配列なのでIPv4/IPv6を両方書ける。詳細は下記「IPv6対応」参照)
   - `server.public_host`: 公開ドメイン
   - `auth.admin_bootstrap`: 初期管理者のユーザ名/パスワード(起動後に変更推奨)
2. ファイアウォールでWebRTCメディア用のUDPポートを開放
   ```bash
   sudo ufw allow 5000:5200/udp   # config.tomlのudp_port_min-udp_port_maxに合わせる
   ```
   (`live-server`自体は内部で`127.0.0.1:8080`にバインドするだけで80/443を直接使わない
   ため、この設定はリバースプロキシの有無に関わらず必要。80/443番の扱いは次項参照)
3. リバースプロキシを設定する(**80/443を既に他のサイトでnginxが使っている場合**は
   新しいserver_nameを追加するだけでよく、ポートの衝突は発生しない)
   - **既存のnginxに追加する場合**:
     ```bash
     sudo cp deploy/nginx/live-server-upgrade-map.conf /etc/nginx/conf.d/
     sudo cp deploy/nginx/live-server.conf /etc/nginx/sites-available/live-server
     sudo ln -s /etc/nginx/sites-available/live-server /etc/nginx/sites-enabled/
     # deploy/nginx/live-server.conf 内の live.example.com を実際のドメインに書き換えてから:
     sudo nginx -t && sudo systemctl reload nginx
     sudo certbot --nginx -d live.example.com   # HTTPS化(任意だが強く推奨)
     ```
   - **新規にCaddyを使う場合**: `deploy/Caddyfile` を参考に `/etc/caddy/Caddyfile` を設定後
     ```bash
     sudo systemctl reload caddy
     ```
4. サービスを起動
   ```bash
   sudo systemctl enable --now live-server
   journalctl -u live-server -f
   ```

### 設定を変更したら

```bash
sudo systemctl restart live-server
```

### 内部ポート(8080)も他で使っている場合

`live-server`自体は`config.toml`の`server.bind`で待ち受けアドレス/ポートを指定するだけ
なので、`127.0.0.1:8080`が別用途で埋まっている場合は空いているポート(例:
`127.0.0.1:18080`)に変更し、リバースプロキシ側の`proxy_pass`/`reverse_proxy`の
向き先もそれに合わせて変更してください。

## IPv6対応

`webrtc.public_ips`は配列なので、IPv4/IPv6のデュアルスタック環境では両方を
列挙できます。

```toml
[webrtc]
public_ips = ["203.0.113.10", "2001:db8::1"]
```

それぞれ自分と同じアドレスファミリのICE候補にのみ1:1 NATマッピングとして
適用されるため、OBS/ブラウザは到達可能な方(多くの場合IPv4)を自動的に選んで
接続します。サーバがIPv6アドレスのみを持つ環境では、IPv6アドレス1つだけを
指定しても動作します。

## 管理画面

- `/dashboard`: 配信者向け。stream_keyの確認/再発行、タイトル/説明/公開設定の変更、
  OBS設定手順の確認、強制停止。
- `/admin`: 管理者向け。ユーザ作成/無効化/削除、配信の作成(配信は管理者が
  ユーザに割り当てる形で作成する)、Webhook登録/テスト送信、サーバ状況の概要。

## Webhook通知

管理画面(`/admin` → Webhookタブ)からDiscordの Incoming Webhook URLを登録すると、
配信開始/終了時に通知が送信されます(embed形式)。Discord以外のURLを登録した場合も
JSON自体は届きますが、embed形式のレンダリングはDiscord向けです。

## トラブルシューティング

- **OBSからの接続が失敗する**: `webrtc.public_ips` が実際にこのサーバの外部到達可能な
  IPと一致しているか確認してください。UDPポート範囲がファイアウォールで開放されて
  いるかも確認してください。
- **映像は来るが再生されない/コーデックエラー**: OBS側のエンコーダがH.264になって
  いるか確認してください(VP8/VP9はこのサーバでは扱いません)。H.264の
  プロファイル(profile-level-id)が特殊な場合、ネゴシエーションに失敗することが
  あります。x264のデフォルト設定であれば通常問題ありません。
- **視聴できるがスマホでは無音**: モバイルブラウザの自動再生ポリシーにより初期状態は
  ミュートです。画面の「ミュート解除」ボタンをタップしてください。

## 開発

```bash
cargo check    # 型チェックのみ(高速)
cargo build --release
```

`frontend/` 配下のHTML/CSS/JSは `rust-embed` によりバイナリへ埋め込まれます。
ビルド工程は不要ですが、変更を反映するには再ビルドが必要です。

`migrations/` はsqlxのマイグレーションで、起動時に自動適用されます。
