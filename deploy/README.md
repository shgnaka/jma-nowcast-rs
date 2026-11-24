# Contabo VPS へのデプロイ

Ubuntu または Debian を想定しています。Docker Engine 28 以降、Docker Compose v2 プラグイン、ホスト上の Caddy 2、Git、OpenSSL を用意します。Docker と Caddy は systemd で自動起動させます。 [Docker のインストール](https://docs.docker.com/engine/install/) · [Caddy のインストール](https://caddyserver.com/docs/install)

## DNS とファイアウォール

`weather.hisho.uk` の A レコードを VPS の公開 IPv4 に向けます。AAAA レコードを使う場合は、その IPv6 でも VPS に到達できるようにします。Contabo 側と OS 側で TCP 80・443 を許可し、管理に使う SSH ポートを維持します。API の 8082 はホストの `127.0.0.1` に公開されます。Caddy が公開側で HTTPS 証明書の取得・更新と HTTP から HTTPS へのリダイレクトを行います。 [Docker のポート公開](https://docs.docker.com/engine/network/port-publishing/) · [Caddy の自動 HTTPS](https://caddyserver.com/docs/automatic-https)

## API を起動する

VPS にこの変更を含むリビジョンをチェックアウトし、リポジトリのルートで実行します。次のコマンドは初回に限り `.env` を作成します。既存の `.env` がある場合は必要な値をエディタで設定し、権限を `0600` にしてください。

```sh
(
  umask 077
  set -C
  api_key=$(openssl rand -hex 32) || exit 1
  {
    printf 'JMA_API_KEY=%s\n' "$api_key"
    sed '/^JMA_API_KEY=/d' .env.example
  } > .env
)
unset JMA_API_KEY JMA_RATE_LIMIT_PER_MINUTE JMA_MAX_CONCURRENT_REQUESTS JMA_FIXED_NOW
sudo systemctl enable --now docker
docker compose config --quiet
docker compose up -d --build --wait
curl -fsS http://127.0.0.1:8082/health
```

Docker を管理できるアカウントで Compose を実行します。`docker compose config --quiet` は展開済みのキーを表示せずに構成を検証します。API コンテナは UID/GID 10001 で動作し、`restart: unless-stopped` により、手動で停止していない限り Docker 再起動後も起動します。Compose がコンテナに渡す環境変数は明示的に列挙しており、`JMA_FIXED_NOW` がコンテナに直接指定された場合も本番モードで起動を拒否します。

## HTTPS を有効にする

新規の Caddy 構成では以下を実行します。すでに Caddy で別のサイトを公開している場合は、既存の `/etc/caddy/Caddyfile` に `weather.hisho.uk` のサイトブロックを追加し、先頭のグローバルオプションにサンプルのタイムアウトとログ設定を統合してください。設定例では、位置や認証ヘッダーを含み得る HTTP アクセスログと HTTP エラーログを既定ロガーから除外しています。このホストを対象にしたアクセスログやリクエストのデバッグログは有効にせず、カスタムロガーにもこれらの HTTP ログを送らないでください。

```sh
sudo install -m 0644 deploy/Caddyfile /etc/caddy/Caddyfile
sudo caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile
sudo systemctl enable --now caddy
sudo systemctl reload caddy
curl -fsS https://weather.hisho.uk/health
```

`GET /health` は認証不要で `{"status":"ok"}` を返します。気象庁への疎通は行いません。`POST /v1/*` の呼び出しでは `Authorization: Bearer <APIキー>` を使い、座標は JSON ボディに含めます。VPS からの確認例では `.env` のキーを URL や curl の引数に含めず、標準入力でヘッダーを渡します。

```sh
sed -n 's/^JMA_API_KEY=/Authorization: Bearer /p' .env |
  curl -fsS https://weather.hisho.uk/v1/precipitation \
    --header @- --header 'Content-Type: application/json' \
    --data '{"lat":35.6812,"lon":139.7671}'
```

## 制限と更新

認証情報が無効な場合は HTTP 401 を返します。リクエスト上限は 1 プロセスあたり 60 秒の固定窓で 60 件、同時実行上限は 4 件です。上限に達するとキューに入れず HTTP 429 と `Retry-After` を返します。アクセス元やエンドポイント別の枠はなく、全 API クライアントで上限を共有します。認証に失敗したリクエストと同時実行上限で拒否されたリクエストは件数に含めません。`.env` の `JMA_RATE_LIMIT_PER_MINUTE` と `JMA_MAX_CONCURRENT_REQUESTS` で正の整数を設定できます。

更新対象のリビジョンに切り替えた後、または `.env` でキーや上限を変更した後は、`docker compose up -d --build --wait` を再実行します。`docker compose ps` と HTTPS 側の `/health` で確認してください。
