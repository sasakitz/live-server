#!/usr/bin/env bash
# live-server インストールスクリプト (Ubuntu Server 24.04想定)
#
# 前提: あらかじめ `cargo build --release` を実行済みであること。
# 実行方法: sudo ./deploy/install.sh
#
# このスクリプトは以下を行う:
#   - 専用の実行ユーザ(live-server, ログイン不可)を作成
#   - /var/lib/live-server (データ) と /etc/live-server (設定) を作成
#   - ビルド済みバイナリを /usr/local/bin/live-server に配置
#   - 設定ファイル例を /etc/live-server/config.toml に配置(既存なら上書きしない)
#   - systemdユニットを配置してdaemon-reload
#
# ファイアウォール設定やリバースプロキシの反映は行わない(このスクリプトの最後に手順を表示する)。

set -euo pipefail

if [ "$(id -u)" -ne 0 ]; then
  echo "root権限で実行してください(例: sudo ./deploy/install.sh)" >&2
  exit 1
fi

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_SRC="$REPO_DIR/target/release/live-server"

if [ ! -x "$BIN_SRC" ]; then
  echo "ビルド済みバイナリが見つかりません: $BIN_SRC" >&2
  echo "先に 'cargo build --release' を実行してください。" >&2
  exit 1
fi

echo "==> 専用ユーザ/グループを作成"
if ! id -u live-server >/dev/null 2>&1; then
  useradd --system --no-create-home --shell /usr/sbin/nologin live-server
  echo "    live-server ユーザを作成しました。"
else
  echo "    live-server ユーザは既に存在します。"
fi

echo "==> ディレクトリを作成"
install -d -o live-server -g live-server -m 750 /var/lib/live-server
install -d -o root -g root -m 755 /etc/live-server

echo "==> バイナリを配置 (/usr/local/bin/live-server)"
install -o root -g root -m 755 "$BIN_SRC" /usr/local/bin/live-server

echo "==> 設定ファイルを配置"
if [ ! -f /etc/live-server/config.toml ]; then
  install -o root -g live-server -m 640 "$REPO_DIR/deploy/config.toml.example" /etc/live-server/config.toml
  echo "    /etc/live-server/config.toml を新規作成しました(要編集)。"
else
  echo "    既に /etc/live-server/config.toml が存在するため上書きしませんでした。"
fi

echo "==> systemdユニットを配置"
install -o root -g root -m 644 "$REPO_DIR/deploy/live-server.service" /etc/systemd/system/live-server.service
systemctl daemon-reload

if ! command -v ffmpeg >/dev/null 2>&1; then
  echo "==> 注意: ffmpegが見つかりません(配信一覧のサムネイル生成に使用します)"
  echo "    'apt install ffmpeg' 等でインストールしてください(必須ではありません)。"
fi

cat <<'EOF'

セットアップが完了しました。残りの手順:

  1. /etc/live-server/config.toml を編集する
     (public_ips, public_host, admin_bootstrapのユーザ名/パスワードは必ず変更)

  2. ファイアウォールでWebRTCメディア用UDPポートを開放する (ufwの例):
       sudo ufw allow 5000:5200/udp   # config.tomlのudp_port_min-udp_port_maxに合わせる
     (80/443/tcpは、既に別サイトでnginx等が使っている場合はそのままでよい。
      新規にリバースプロキシを立てる場合のみ別途開放すること)

  3. リバースプロキシを設定する:
       - 既存nginxに追加する場合: deploy/nginx/ 配下の設定例を参照
       - 新規にCaddyを使う場合: deploy/Caddyfile を参照

  4. live-serverを起動:
       sudo systemctl enable --now live-server
       journalctl -u live-server -f

EOF
