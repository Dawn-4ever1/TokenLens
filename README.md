# TokenLens

A desktop dashboard for the tokens your AI coding agents use: Claude Code,
Codex, Gemini CLI, Qoder, OpenCode, Kimi, Qwen Code and more, on Windows and
macOS.

[简体中文](README.zh-CN.md)

- **Where the tokens go**: input, output, cache read, cache write and reasoning
  are shown separately, because cache reads are usually over 90% of the total
  and are not what you pay for.
- **Trends and sessions**: daily and hourly trends, an hour × weekday heatmap,
  every session with its per-turn breakdown, filters by agent, model, project
  and computer.
- **Cost estimates**: "API-equivalent" cost from the public LiteLLM,
  OpenRouter and models.dev price catalogs, refreshed daily. Not your bill:
  subscriptions and relays can cost very differently.
- **One computer or many**: works on its own, or syncs several computers to a
  server you host.
- **Light in the background**: a tray app that reads agent logs only when they
  change; the window is opened on demand.

> Status: early (0.x). The interface is in Simplified Chinese for now.

## Download

Get the latest installer from [Releases](https://github.com/Dawn-4ever1/TokenLens/releases):

| System | File |
|---|---|
| Windows 10 / 11 | `TokenLens_<version>_x64-setup.exe` |
| macOS 11+ on Apple silicon | `TokenLens_<version>_aarch64.dmg` |

The installers are not code-signed yet, so the system asks once:

- **Windows**: on "Windows protected your PC", click "More info", then "Run anyway".
- **macOS**: open the app from Applications; if it cannot be verified, go to
  System Settings → Privacy & Security and click "Open Anyway".

## Two ways to use it

When the app first opens, pick where the data lives:

- **Only on this computer** (只在这台电脑上): no server, no account. The app
  runs the same sync server code in process and keeps the data in a local
  SQLite file. It goes online only to download the public price catalogs once a
  day.
- **Connect to a server** (连接服务器): several computers on one dashboard.
  Host the server yourself with Docker, see [deploy/README.md](deploy/README.md),
  and pair each computer with a one-time code.

You can switch from local to a server later; the computer's history is
uploaded when it pairs.

## Privacy

- Only numbers leave the agent logs: token counts, model names, times, and a
  one-way hash of the project. Prompts, answers, code, file contents, paths and
  session titles are never read into the database or uploaded.
- Server tokens are stored in the macOS Keychain or Windows Credential Manager
  and never reach the web view; the server stores only their hashes.
- In local mode nothing is uploaded at all.

## Supported agents

Claude Code, Codex, Gemini CLI, OpenCode, Amp, Kilo Code, Roo Code, Droid,
Kimi, Qwen Code, GitHub Copilot, OpenClaw, ZCode, DeepSeek Harness, WorkBuddy,
Antigravity, and Qoder (international). Log parsing comes from
[tokscale](https://github.com/junhoyeo/tokscale), used as a library at a pinned
revision; Qoder has its own adapter, and because its local logs mostly record
credits, its token numbers are marked as estimates.

## Build from source

Prerequisites: Rust (stable), Node.js 24, and the
[Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/) for your
system.

```sh
cd app
npm ci
npm run tauri dev        # desktop app with hot reload
npm run tauri build      # installer for this system
```

Tests:

```sh
cargo test --workspace   # collector, server, app shell
cd app && npm test       # unit tests
npm run test:e2e         # UI tests in Chrome (Playwright, mock data)
```

| Path | What it is |
|---|---|
| `app/` | Tauri 2 desktop app: React + TypeScript UI in `src/`, Rust shell in `src-tauri/` |
| `crates/collector-core/` | Reads agent logs (tokscale + Qoder adapter), queues and uploads usage events |
| `crates/usage-event/` | The usage event format shared by every part (`schema/usage-event.v1.json`) |
| `collector/` | Command-line collector for testing and headless machines |
| `server/` | The sync server (axum + SQLite): pairing, ingest, dashboard API, pricing |
| `deploy/` | Docker, Caddy, backup and restore scripts for self-hosting |
| `scripts/` | Release and packaging scripts; `reconcile/` compares numbers with a Token Monitor Hub |

Code comments mention design decisions as `D0xx`; those notes are not part of
this repository.

## Credits

- [tokscale](https://github.com/junhoyeo/tokscale) (MIT) parses the agent logs
  and prices events.
- The colour approach follows [TokenTracker](https://github.com/xiufengsun/TokenTracker) (MIT).
- Agent and vendor marks belong to their owners and are used only for
  identification; see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## License

[MIT](LICENSE)
