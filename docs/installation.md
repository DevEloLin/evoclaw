# Installation

EvoClaw is a single Rust workspace with five core crates plus one optional gateway crate. There is no installer — you build from source, then optionally copy the resulting binaries onto your `$PATH`.

This page assumes you have **never seen this repo before**.

---

## 1. Prerequisites

| Tool | Minimum | Why |
|------|---------|-----|
| Rust | 1.80 (stable) | core language; `rust-toolchain.toml` pins `stable` |
| `cargo` | bundled with Rust | build / test / run |
| A Unix-like shell | bash 3.2 / zsh / fish | `scripts/check.sh` is bash-3.2 compatible |
| An OpenAI-compatible API key | DeepSeek, Kimi, Qwen, OpenRouter, Ollama (`""`), … | needed at runtime, not at build time |

If you don't have Rust:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustup default stable
```

The repo's `rust-toolchain.toml` pins `channel = "stable"`, so once Rust is installed `cargo` will auto-download the right toolchain on first build.

---

## 2. Get the source

```bash
git clone <repo-url> evoclaw
cd evoclaw
```

Layout you should see:

```
.
├── Cargo.toml
├── rust-toolchain.toml
├── README.md
├── crates/
│   ├── evo-cli/             # binary `evo`
│   ├── evo-core/            # agent loop, session, learning, prompt
│   ├── evo-tools/           # 7-of-10 atomic tools
│   ├── evo-providers/       # OpenAI-compat HTTP client + ToolFingerprint
│   ├── evo-policy/          # permissions + cost engine
│   └── evo-mock-provider/   # dev-only deterministic mock
├── crates-ext/
│   └── evo-gateway/         # binary `evo-gateway` (optional HTTP daemon)
├── docs/                    # user docs (this directory)
├── prd/                     # product specs
│   ├── prd.md
│   ├── architecture.html
│   ├── design.html
│   └── plan/                # dev-time plan, prompt templates
└── scripts/check.sh         # CI gates
```

---

## 3. Build

```bash
cargo build --workspace --release
```

First build downloads ~80 dependency crates (~80 MB) and compiles for ~30 seconds on a modern machine. Resulting binaries:

- `target/release/evo` — main CLI
- `target/release/evo-gateway` — optional local HTTP daemon

Verify the build:

```bash
./target/release/evo --help
```

You should see seven subcommands: `onboard`, `run`, `doctor`, `replay`, `skill`, `memory`, `gateway` (plus `doctor-of`).

---

## 4. Install to `$PATH` (optional)

```bash
# pick one
cargo install --path crates/evo-cli         # installs to ~/.cargo/bin
cargo install --path crates-ext/evo-gateway # installs evo-gateway too

# or just symlink
ln -s "$(pwd)/target/release/evo" /usr/local/bin/evo
```

`~/.cargo/bin` is already on your `$PATH` if you used `rustup`.

---

## 5. First-run setup — interactive wizard

Just type `evoclaw` (no args) and you'll be walked through provider selection automatically. Or invoke the wizard explicitly:

```bash
evoclaw onboard
```

The wizard:

1. Lists 7 providers (DeepSeek / Kimi / Qwen / OpenAI / OpenRouter / Ollama / Custom).
2. Optionally opens your browser at the provider's API-key page (Y/n).
3. Asks you to paste the key.
4. Writes `~/.evoclaw/config.toml` (provider id + base_url + model) and `~/.evoclaw/secrets/<provider>.key` with **chmod 600**.

To switch provider later or rotate the key:

```bash
evoclaw login        # CLI subcommand
# or, inside the REPL:
evoclaw> /login
```

**Key resolution order** at runtime (PRD §13.2):

1. `EVO_API_KEY` env var (highest priority, for CI / scripts)
2. `~/.evoclaw/secrets/<active-provider>.key` (chmod 600 plain text)
3. error → user is prompted to run `evoclaw login`

The wizard creates this layout under `~/.evoclaw/`:

```
~/.evoclaw/
├── config.toml           # default model + budget + security
├── workspace/            # tool sandbox
├── logs/                 # session JSONL
├── skills/               # learned skill YAML files
├── browser_profiles/     # (Phase 4.5; reserved)
├── secrets/              # local key vault
├── plugins/              # reserved
└── cache/                # transient
```

Default `config.toml`:

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

Switch provider by editing `model.default` and `model.base_url`. Any OpenAI-compatible endpoint works — DeepSeek, Kimi, Qwen, OpenRouter, vLLM, llama.cpp's server mode, Ollama (`http://localhost:11434/v1`), etc.

---

## 6. (Optional) Override the key with an env var

If you completed the wizard above, you already have a key on disk. The env var path is for **CI, scripts, or one-off overrides**:

```bash
export EVO_API_KEY=sk-your-key-here   # takes precedence over ~/.evoclaw/secrets/*.key
```

For local providers like Ollama, set `EVO_API_KEY=local` or skip — the wizard treats local providers as keyless.

---

## 7. Verify

```bash
evo doctor
```

Expected output:

```
== evo doctor ==
home    : /Users/you/.evoclaw
config  : OK (.../config.toml)
model   : deepseek-chat via https://api.deepseek.com/v1
workspace: /Users/you/.evoclaw/workspace
logs    : /Users/you/.evoclaw/logs
api_key : set (len=51)
```

If `api_key: MISSING`, your `EVO_API_KEY` env var didn't propagate — re-source your shell rc. You're done. Next: **[Getting started](getting-started.md)**.

---

## 8. Updating

```bash
git pull
cargo build --workspace --release
```

EvoClaw stores all state in `~/.evoclaw/`, so a rebuild never wipes your skills or memory. To wipe and start over:

```bash
rm -rf ~/.evoclaw
evo onboard
```

---

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| `error: rustc x.y.z is not supported` | `rustup update stable` |
| Build fails on `reqwest` / `openssl` | install `pkg-config` and `openssl` (Linux) or `brew install openssl` (macOS) |
| `evo` not found after `cargo install` | check `~/.cargo/bin` is on your `$PATH` |
| `EVO_API_KEY env var not set` | re-source shell rc; verify with `echo $EVO_API_KEY` |
| `MaxTurns(25)` error during a run | task too long for default; simplify prompt or raise `RuntimeConfig::max_turns` |
