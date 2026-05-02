# 安装

EvoClaw 是单个 Rust workspace，含 5 个核心 crate + 1 个可选 gateway crate。**没有安装器**，你需要自己编译，再（可选）把产出的二进制复制到 `$PATH` 上。

本页假设你**完全没看过这个仓库**。

---

## 1. 准备条件

| 工具 | 最低版本 | 用途 |
|------|---------|------|
| Rust | 1.80（stable） | 主语言；`rust-toolchain.toml` 已锁 `stable` |
| `cargo` | 跟 Rust 一起 | 构建 / 测试 / 运行 |
| 类 Unix shell | bash 3.2 / zsh / fish | `scripts/check.sh` 兼容 bash 3.2 |
| OpenAI 兼容 API key | DeepSeek / Kimi / Qwen / OpenRouter / Ollama（`""`）等 | 运行时需要，编译时不需要 |

如果你还没装 Rust：

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustup default stable
```

仓库的 `rust-toolchain.toml` 已设 `channel = "stable"`，第一次 `cargo` 会自动下载对应工具链。

---

## 2. 取代码

```bash
git clone <repo-url> evoclaw
cd evoclaw
```

应该看到的目录：

```
.
├── Cargo.toml
├── rust-toolchain.toml
├── README.md
├── crates/
│   ├── evo-cli/             # 二进制 `evo`
│   ├── evo-core/            # agent loop / session / 学习 / prompt
│   ├── evo-tools/           # 7-of-10 原子工具
│   ├── evo-providers/       # OpenAI 兼容 HTTP 客户端 + ToolFingerprint
│   ├── evo-policy/          # 权限 + Cost Engine
│   └── evo-mock-provider/   # 仅 dev：确定性 mock
├── crates-ext/
│   └── evo-gateway/         # 二进制 `evo-gateway`（可选 HTTP daemon）
├── docs/                    # 用户文档
├── prd/                     # 规格
│   ├── prd.md
│   ├── architecture.html
│   ├── design.html
│   └── plan/                # 开发期计划、提示词
└── scripts/check.sh         # CI 门
```

---

## 3. 编译

```bash
cargo build --workspace --release
```

首次构建会下载 ~80 个依赖（约 80 MB），现代机器上 ~30 秒。产物：

- `target/release/evo` — 主 CLI
- `target/release/evo-gateway` — 可选的本地 HTTP daemon

验证：

```bash
./target/release/evo --help
```

应当显示 7 个子命令：`onboard`、`run`、`doctor`、`replay`、`skill`、`memory`、`gateway`（外加 `doctor-of`）。

---

## 4. 安装到 `$PATH`（可选）

```bash
# 任选一种
cargo install --path crates/evo-cli         # 装到 ~/.cargo/bin
cargo install --path crates-ext/evo-gateway # 顺带装上 evo-gateway

# 或直接 symlink
ln -s "$(pwd)/target/release/evo" /usr/local/bin/evo
```

如果你用 `rustup` 装的 Rust，`~/.cargo/bin` 已在 `$PATH` 上。

---

## 5. 第一次运行 — 交互式 wizard

直接输入 `evoclaw`（不带任何参数），会自动检测无配置并启动厂商选择 wizard。也可显式调用：

```bash
evoclaw onboard
```

Wizard 流程：

1. 列出 7 个厂商（DeepSeek / Kimi / Qwen / OpenAI / OpenRouter / Ollama / 自定义）
2. 可选地用浏览器打开该厂商的 API key 页面（Y/n）
3. 让你粘贴 key
4. 写入 `~/.evoclaw/config.toml`（含 provider id + base_url + model）和 `~/.evoclaw/secrets/<provider>.key`，**自动 chmod 600**

之后切换厂商或轮换 key：

```bash
evoclaw login        # CLI 子命令
# 或在 REPL 内：
evoclaw> /login
```

**API key 解析优先级**（PRD §13.2）：

1. `EVO_API_KEY` 环境变量（最高优先；CI / 脚本最方便）
2. `~/.evoclaw/secrets/<active-provider>.key`（wizard 写的位置，chmod 600 纯文本）
3. error → 提示用户跑 `evoclaw login`

Wizard 创建的 `~/.evoclaw/` 布局：

```
~/.evoclaw/
├── config.toml           # 默认模型 / 预算 / 安全
├── workspace/            # 工具沙箱
├── logs/                 # session JSONL
├── skills/               # 学到的 skill YAML
├── browser_profiles/     # （Phase 4.5 预留）
├── secrets/              # 本地凭证
├── plugins/              # 预留
└── cache/                # 临时
```

默认 `config.toml`：

```toml
[model]
default = "deepseek-chat"
base_url = "https://api.deepseek.com/v1"
fallback = ["qwen-plus", "kimi-k2"]

[budget]
per_task_usd  = 0.5
per_day_usd   = 5.0
per_month_usd = 100.0

[security]
default_permission  = "P1"
high_risk_intercept = true
```

切换厂商只要改 `model.default` + `model.base_url`。任何 OpenAI 兼容端点都行：DeepSeek / Kimi / Qwen / OpenRouter / vLLM / llama.cpp 的 server 模式 / Ollama（`http://localhost:11434/v1`）等。

---

## 6.（可选）用环境变量覆盖

如果上一步 wizard 已经写好了 key 文件，这步可跳过。Env var 路径主要用于 **CI、脚本、临时覆盖**：

```bash
export EVO_API_KEY=sk-your-key-here   # 优先级高于 ~/.evoclaw/secrets/*.key
```

本地厂商（如 Ollama）wizard 会跳过 key 写入，无需 env var。

---

## 7. 验证

```bash
evo doctor
```

期望输出：

```
== evo doctor ==
home    : /Users/you/.evoclaw
config  : OK (.../config.toml)
model   : deepseek-chat via https://api.deepseek.com/v1
workspace: /Users/you/.evoclaw/workspace
logs    : /Users/you/.evoclaw/logs
api_key : set (len=51)
```

如果看到 `api_key: MISSING`，说明 `EVO_API_KEY` 没生效——重新 source shell rc。完成。下一步：**[快速上手](getting-started.md)**。

---

## 8. 升级

```bash
git pull
cargo build --workspace --release
```

EvoClaw 所有状态都在 `~/.evoclaw/` 下，重新编译不会清掉技能或记忆。要彻底重置：

```bash
rm -rf ~/.evoclaw
evo onboard
```

---

## 故障排查

| 现象 | 解决 |
|------|------|
| `error: rustc x.y.z is not supported` | `rustup update stable` |
| `reqwest` / `openssl` 编译失败 | Linux 装 `pkg-config` + `openssl`；macOS 用 `brew install openssl` |
| `cargo install` 后找不到 `evo` | 检查 `~/.cargo/bin` 是否在 `$PATH` |
| `EVO_API_KEY env var not set` | 重新 source shell rc，用 `echo $EVO_API_KEY` 确认 |
| 跑长任务时报 `MaxTurns(25)` | 简化 prompt 或调高 `RuntimeConfig::max_turns` |
