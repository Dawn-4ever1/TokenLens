# Official agent and model-vendor marks

Small identity marks shown beside agent and model names (D041 follow-up,
2026-11-12). Each file comes from the product's own website, its official
GitHub organisation, or the icon of its official desktop app; they are bundled
locally so the dashboard never fetches them at run time. The marks belong to
their owners and are used only to identify each tool. Raster files were scaled
to 64×64 PNG; SVGs are unchanged.

Claude, OpenAI and Copilot are drawn inline in `../../components/brandMarks.tsx`
(sources noted there) so they can follow the app's light/dark colours. Pi and
Hermes keep their original monochrome SVG, with a theme-only CSS invert.
Raster additions are used at their published size rather than being recompressed.

| File | Used for | Source (fetched 2026-11-12) |
|---|---|---|
| `qoder.png` | Qoder agent, Qoder models | Icon of the official Qoder desktop app for Windows (256 px frame of its shortcut icon); same mark as https://qoder.com |
| `workbuddy.png` | WorkBuddy | Icon embedded in the official WorkBuddy Windows app (`WorkBuddy.exe`, 256 px) |
| `antigravity.png` | Antigravity (app, CLI and IDE extension) | Icon embedded in the official Antigravity Windows app (`Antigravity.exe`, 256 px) |
| `deepseek.png` | DSH (DeepSeek Harness, `deepseek-ai/deepseek-harness`), DeepSeek models | https://www.deepseek.com/favicon.ico (225 px frame) |
| `zcode.png` | ZCode agent | https://zcode.z.ai/favicon-192x192.png |
| `zai.svg` | 智谱 / Z.ai models (GLM) | https://z-cdn.chatglm.cn/z-ai/static/logo.svg (icon of chat.z.ai) |
| `openclaw.svg` | OpenClaw | https://openclaw.ai/favicon.svg |
| `gemini.png` | Gemini CLI, Google models | https://gemini.google.com (site icon, `gemini_sparkle_4g_512_lt`) |
| `cursor.svg` | Cursor | https://cursor.com/marketing-static/favicon.svg |
| `opencode.svg` | OpenCode | https://opencode.ai/favicon.svg |
| `amp.svg` | Amp | https://ampcode.com/favicon.svg |
| `kilo.svg` | Kilo Code | https://kilo.ai/favicon/favicon.svg |
| `roocode.png` | Roo Code | https://github.com/RooCodeInc.png (official GitHub organisation) |
| `factory.svg` | Droid (Factory) | https://factory.com/favicon.svg |
| `kimi.png` | Kimi CLI, Moonshot models | https://www.kimi.com/pwa-192.png |
| `qwen.png` | Qwen Code, Qwen models | https://github.com/QwenLM.png (official GitHub organisation) |
| `mistral.svg` | Mistral models | https://mistral.ai/favicon.svg |
| `xai.png` | xAI models (Grok) | https://x.ai/icon.png |
| `meta.png` | Meta models (Llama) | https://github.com/facebookresearch.png (official GitHub organisation) |
| `cohere.png` | Cohere models | https://cohere.com/apple-touch-icon.png |

### Agent icon expansion (T50, fetched 2026-11-17)

| File | Used for | Source |
|---|---|---|
| `cline.png` | Cline | Cline official product mark, bundled in the read-only TokenTracker reference |
| `trae.svg` | Trae | https://www.trae.ai/favicon.svg (same product artwork via reference) |
| `kiro.svg` | Kiro | https://kiro.dev (official Kiro mark, via reference) |
| `augment.png` | Augment Code | https://github.com/augmentcode.png (verified official organisation) |
| `codebuddy.svg` | CodeBuddy | https://www.codebuddy.ai (official product artwork, via reference) |
| `zed.webp` | Zed Agent | Zed official brand; https://zed.dev/brand (distributed by tokscale) |
| `warp.png` | Warp | https://github.com/warpdotdev.png (official organisation) |
| `goose.png` | Goose | https://github.com/aaif-goose/goose (official Goose artwork via tokscale) |
| `junie.svg` | Junie | https://plugins.jetbrains.com/files/26104/1198154/icon/default.svg (official JetBrains plugin) |
| `crush.png` | Crush | https://github.com/charmbracelet/crush (official Charm product artwork via tokscale) |
| `grok.svg` | Grok CLI / Build | Official Grok product mark (via reference) |
| `devin.svg` | Devin CLI / Desktop | https://app.devin.ai (official app mark, vectorised in reference) |
| `lmstudio.svg` | LM Studio | https://lmstudio.ai (official product artwork, via reference) |
| `cherrystudio.png` | Cherry Studio | https://github.com/CherryHQ/cherry-studio (official product artwork via tokscale) |
| `codebuff.png` | Codebuff | https://github.com/CodebuffAI/codebuff (official product artwork via tokscale) |
| `hermes.svg` | Hermes Agent | Official Hermes product artwork, via reference |
| `pi.svg` | Pi | https://pi.dev/logo.svg (original mark retained in reference) |

The same `ClientName` component is used by overview, session, filter and device
views. Kilo CLI shares `kilo.svg`; Qoder CN shares `qoder.png`. Unknown
new agents retain the generic bot mark. No runtime network request is used.
