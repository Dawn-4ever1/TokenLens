# Self-hosting the sync server

You only need a server to see **several computers on one dashboard**. A single
computer works without one: choose "只在这台电脑上" (only on this computer) when
the app first opens.

The server (`tokenlens-server`) is one Rust binary with an SQLite database. It
receives usage numbers from the desktop apps, answers the dashboard, and
estimates costs from public price catalogs. This folder runs it in Docker behind
a TLS proxy (Caddy here; any proxy that terminates HTTPS works).

| File | What it does |
|---|---|
| `Dockerfile` | Builds the server image from the repository root (only `Cargo.toml`, `Cargo.lock`, `schema/`, `config/pricing-overrides.json`, `crates/usage-event/`, `server/` are used) |
| `compose.yaml` | Runs it; the port is published on `127.0.0.1:8787` only; data in `./data`, backups in `./backups` |
| `Caddyfile.example` | A site block for Caddy (HTTPS and reverse proxy) |
| `backup.sh` | Online backup into `./backups`, keeps 30 days |
| `restore-drill.sh` | Restores the newest backup into a scratch folder and starts a throwaway server on it, without touching the running one |

## Requirements

- A Linux host with Docker and the Compose plugin. 1 GB of free memory is
  enough to run it; building needs more (see "Resources").
- A domain name whose DNS points at the host, and a TLS proxy. The apps refuse
  plain `http://` except for `localhost`.
- Outbound HTTPS to `raw.githubusercontent.com`, `openrouter.ai` and
  `models.dev` for the daily price catalogs (optional, see "Cost estimates").

## 1. Build and start

```sh
git clone https://github.com/Dawn-4ever1/TokenLens.git
cd TokenLens/deploy
sudo mkdir -p data backups && sudo chown 10001:10001 data backups   # the container runs as uid 10001
docker compose up -d --build
docker compose ps                       # should become "healthy"
curl -s http://127.0.0.1:8787/healthz   # {"status":"ok"}
```

On a small host, build with one compile job so the build does not starve other
services of memory:

```sh
docker compose build --build-arg BUILD_JOBS=1 && docker compose up -d
```

Optional `.env` in this folder (both have defaults):

```sh
DASHBOARD_TIMEZONE=Asia/Shanghai   # day buckets on the dashboard (IANA name)
RUST_LOG=info
```

The listen address and database path are fixed in `compose.yaml` so `.env`
cannot expose the port to the internet.

## 2. HTTPS

Copy the block from `Caddyfile.example` into your Caddyfile, replace
`dashboard.example.com` with your domain, then:

```sh
caddy validate --config /etc/caddy/Caddyfile && caddy reload --config /etc/caddy/Caddyfile
curl -s https://dashboard.example.com/healthz   # {"status":"ok"}
```

## 3. Pair computers

```sh
docker compose exec dashboard tokenlens-server pair-code create
```

The code is printed only in this terminal, is valid for 10 minutes and works
once. In the app choose "连接服务器" (connect to a server) and enter
`https://<your domain>` and the code. Further computers get their codes from an
already paired app (Devices → 添加设备), without logging in to the server.

```sh
docker compose exec dashboard tokenlens-server device list
docker compose exec dashboard tokenlens-server device revoke <device id>   # both tokens stop working at once
```

### Separate spaces (tenants)

Each tenant is an isolated space: computers in one tenant share a dashboard and
never see another tenant's data. Existing data lives in the personal tenant.
To give someone their own space:

```sh
docker compose exec dashboard tokenlens-server tenant create acme "Acme team"
docker compose exec dashboard tokenlens-server pair-code create --tenant acme
```

There are no user accounts yet: a computer's tenant is fixed by the code it was
paired with.

## 4. Cost estimates

The server downloads three public price catalogs (LiteLLM, OpenRouter,
models.dev) once a day into `data/tokscale/` (about 15 MB, safe to delete) and
re-prices stored events when they change. Costs are "API-equivalent"
estimates, not your bill.

```sh
docker compose exec dashboard tokenlens-server pricing refresh   # download now
docker compose restart dashboard                                 # use the new catalogs at once (otherwise within 24 h)
docker compose exec dashboard tokenlens-server pricing report    # which models have prices
```

Without internet access set `PRICING_REFRESH=off` in `.env` and copy
`pricing-*.json` files into `data/tokscale/cache/` yourself; otherwise costs
stay "unpriced".

## 5. Backups

Run as root in this folder (backups belong to the container user, mode 600):

```sh
./backup.sh
./restore-drill.sh      # ends with "Restore drill passed."
```

Daily at 04:15 (host time), in root's crontab:

```cron
15 4 * * * /path/to/TokenLens/deploy/backup.sh >> /path/to/TokenLens/deploy/backups/backup.log 2>&1
```

Backups run while the server keeps serving; each file is integrity-checked
before it is kept. Copy `backups/` off the host regularly: a backup on the same
disk does not survive losing the host.

## 6. Restore

```sh
docker compose stop dashboard
docker compose run --rm --no-deps dashboard tokenlens-server restore /app/backups/<file>.sqlite
docker compose start dashboard
```

`restore` checks the backup first and keeps the replaced database as
`data/tokenlens.sqlite.before-restore-<time>.sqlite`. After a restore the
server gets a new store id, and every paired app re-sends its full history on
its next upload or heartbeat, so events newer than the backup come back as long
as the computers still have their agent logs. Computers paired after the backup
must pair again.

## 7. Upgrade

Pull or copy the new code (`data/` and `backups/` are never overwritten), then:

```sh
./backup.sh
docker compose build                     # add --build-arg BUILD_JOBS=1 on small hosts
./restore-drill.sh                       # the new image against the newest backup
docker compose up -d --no-build          # about one second of downtime
```

Before a schema change the server saves `data/*.pre-migration-<version>.sqlite`
by itself. **Upgrade the server before the apps**: a newer app may send fields
an older server rejects.

## Resources

- Running: the image is about 150 MB; the database grows by about 8 MB per
  10,000 events; up to 30 daily backups are kept.
- Building: a Rust toolchain image (about 1 GB) plus compile cache; the server
  includes tokscale's pricing engine and builds a bundled OpenSSL, so expect
  10–15 minutes with one job and a peak of about 1.5 GB of memory.
  `docker builder prune -f` and `docker image rm rust:1-slim-bookworm` free the
  space afterwards; the next build then starts from scratch.
