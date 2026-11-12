# TokenLens

看清 AI 编程助手用了多少 token 的桌面看板：Claude Code、Codex、Gemini CLI、Qoder、OpenCode、Kimi、Qwen Code 等，支持 Windows 和 macOS。

[English](README.md)

- **token 花在哪**：输入、输出、缓存读取、缓存写入、推理分开显示。缓存读取通常占九成以上，并不等于你付费的部分。
- **趋势和会话**：按天、按小时的趋势，「星期 × 小时」热力图，每个会话逐轮的 token 构成，可按工具、模型、项目、电脑筛选。
- **成本估算**：按 LiteLLM、OpenRouter、models.dev 三份公开价格表估算的「API 等价成本」，每天更新。不是账单：订阅制、中转站的实际花费可能差很多。
- **一台或多台电脑**：一台电脑直接用；多台电脑可以同步到你自己搭的服务器上合在一起看。
- **后台占用小**：常驻托盘，agent 日志有变化时才去读；窗口需要时再打开。
- **应用内更新**：自动检查公开 GitHub 版本，可在设置里关闭；点「下载并更新」后才下载、验签、安装和重启。

> 状态：早期版本（0.x），界面目前只有简体中文。

## 下载

在 [Releases](https://github.com/Dawn-4ever1/TokenLens/releases) 下载最新安装包：

| 系统 | 文件 |
|---|---|
| Windows 10 / 11 | `TokenLens_<版本>_x64-setup.exe` |
| macOS 11 及以上（Apple 芯片） | `TokenLens_<版本>_aarch64.dmg` |

安装包暂时没有付费签名，系统第一次会拦一下：

- **Windows**：出现「Windows 已保护你的电脑」时，点「更多信息」，再点「仍要运行」。
- **macOS**：从「应用程序」里打开；提示无法验证开发者时，到「系统设置 → 隐私与安全性」点「仍要打开」。

## 两种用法

第一次打开时选数据放在哪里：

- **只在这台电脑上**：不需要服务器，也不需要账号。应用在自己内部运行同一套服务端代码，数据存在本机的 SQLite 文件里；每天下载公开价格表，启用自动检查时还会检查应用更新。
- **连接服务器**：几台电脑合在一起看。用 Docker 自己搭服务端（见 [deploy/README.md](deploy/README.md)，英文），每台电脑用一次性配对码连上。

以后可以从「只在这台电脑上」改为连接服务器，配对后这台电脑的历史会一起上传。

## 隐私

- 从 agent 日志里只取数字：token 数、模型名、时间，以及项目的单向哈希。提示词、回答、代码、文件内容、路径和会话标题都不会进数据库，也不会上传。
- 连接服务器用的凭据存在 macOS 钥匙串或 Windows 凭据管理器里，看板页面拿不到；服务端只保存它们的哈希。
- 「只在这台电脑上」时什么都不上传。
- 更新检查在启动约 10 秒后、之后每 24 小时匿名 GET 公开 GitHub Release，不发送用量或凭据；可在设置里关闭，也可手动检查。更新包安装前必须验签，这与系统的付费代码签名不同。

## 支持的 agent

Claude Code、Codex、Gemini CLI、OpenCode、Amp、Kilo Code、Roo Code、Droid、Kimi、Qwen Code、GitHub Copilot、OpenClaw、ZCode、DeepSeek Harness、WorkBuddy、Antigravity，以及 Qoder 国际版。日志解析用的是 [tokscale](https://github.com/junhoyeo/tokscale)（作为库使用，锁定版本）；Qoder 有单独的适配，它本地大多只记 credits，所以 token 数标为「估算」。

## 从源码构建

需要 Rust（stable）、Node.js 24，以及对应系统的 [Tauri 2 依赖](https://v2.tauri.app/start/prerequisites/)。

```sh
cd app
npm ci
npm run tauri dev        # 开发模式
npm run tauri build      # 打本系统的安装包
```

测试：

```sh
cargo test --workspace   # 采集端、服务端、应用外壳
cd app && npm test       # 前端单元测试
npm run test:e2e         # 界面测试（Chrome，Playwright，模拟数据）
```

目录说明见英文版 README。代码注释里的 `D0xx` 指设计决策记录，这些记录不在本仓库里。

## 致谢

- [tokscale](https://github.com/junhoyeo/tokscale)（MIT）：解析 agent 日志、估算价格。
- 配色思路参考 [TokenTracker](https://github.com/xiufengsun/TokenTracker)（MIT）。
- 各 agent 和模型厂商的标志归各自所有者，只用来区分工具，见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。

## 许可

[MIT](LICENSE)
