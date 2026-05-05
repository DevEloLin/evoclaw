# Getting started — zero to your first running agent

Already installed? If not, start with **[Installation](installation.md)** and come back.

This walkthrough takes ~5 minutes and covers:

1. Run a first task
2. Watch EvoClaw learn from it (a Skill is born)
3. Re-run a similar task and see the Skill kick in
4. Inspect what happened (replay, doctor, memory)
5. Optional: open the WebChat interface

---

## 0. Sanity check

```bash
evoclaw --help     # subcommand list, plus interactive default
evoclaw doctor     # should print api_key: set (len=...)
```

`evo` is a 3-letter alias of `evoclaw` — both binaries do the same thing.

---

## 0.5. Optional but recommended — register your secrets

If you plan to ask the agent to do anything that involves a real credential, register it in the local vault first. The vault keeps the raw value on your disk; the model only ever sees a `${SECRET:NAME}` placeholder.

```bash
evoclaw secret add github_pat ghp_yourActualValueHere
evoclaw secret list
```

Verify the redactor before you trust it:

```bash
evoclaw secret test "use ghp_yourActualValueHere to clone the repo"
output : use ${SECRET:github_pat} to clone the repo
hits   : 1 substitution(s)
```

Even **without** any vault entries, common credential shapes (`sk-*`, `ghp_*`, `eyJ*`, `AKIA*`, plus high-entropy 32+ char tokens) are caught by a pattern fallback and rewritten as `[REDACTED:<kind>:<8-char-fingerprint>]`. So leaking a key by accident is hard; registering one explicitly makes the placeholder *named* and the substitution *deterministic*.

---

## 1. Enter the interactive shell

Just type the project name, **no subcommand**, exactly like `claude` or `codex`:

```bash
evoclaw
```

You drop into the welcome panel:

```
───────────────────────────────────────────────────────────────────
  \\  ▄   ▄  //                  Quick start
    ▄███████▄                    ──────────────────────────
    █       █                    /help    list all commands
    █ ▀▀ ▀▀ █                    /login   configure auth
    ▀█▄▄▄▄▄█▀                    /doctor  health check
      ▄▄ ▄▄                      /skill   browse skills
  //  ██ ██  \\
                                  Status
  EvoClaw  v0.3.7                 ──────────────────────────
  self-evolving agent runtime     auth    ✓ ready
                                  model   deepseek-chat

  deepseek  ·  deepseek-chat
  ~/.evoclaw                      Ctrl-D to exit  ·  /help

  ✓ ready  secrets file: ~/.evoclaw/secrets/deepseek.key

───────────────────────────────────────────────────────────────────

─ input ───────────────────────────────────────────────────────────
  ▷ Type your message and press Enter to send  ·  /help for commands
───────────────────────────────────────────────────────────────────
shortcuts: Tab /cmd  ·  ↑↓/Ctrl-P/N history  ·  Ctrl-R search  ·  Ctrl-C quit
```

Type a plain-language task at the prompt:

```
evoclaw> list every Cargo.toml under the workspace and write a one-line summary
         to ~/.evoclaw/workspace/cargo-toml-summary.txt
```

Abridged output:

```
→ running... session log: /Users/you/.evoclaw/logs/task-20260502T143012.481.jsonl

=== final (4 turns) ===
Wrote 7 paths to cargo-toml-summary.txt. Roots: evoclaw, my-other-repo, ...
```

The agent decided on its own to use `list_dir` + `read_file` + `write_file`. There were no Skills available yet, so it explored.

---

## 2. Watch a Skill get created

Right after the task ends, EvoClaw runs a **reflection round** (PRD §11). It asks the model "what just happened, what's reusable?" and saves the answer to:

- **Memory L3** — free-text record (`~/.evoclaw/memory/L3.jsonl`)
- **Skill DRAFT** — structured YAML (`~/.evoclaw/skills/skill-*.yaml`)

```bash
evo skill list
```

Expected:

```
ID                       STATE      SCORE VER      NAME
skill-20260502T1430      DRAFT       0.50 v1       enumerate cargo manifests
```

The skill is in **DRAFT** state with score 0.50. It needs a sandbox pass and ≥3 successful real uses to climb to **ACTIVE** (PRD §32 EWMA rules).

```bash
evo skill show skill-20260502T1430
```

---

## 3. Re-run a similar task

```bash
evo run "find all Cargo manifests in the workspace and summarise"
```

Expected behaviour: EvoClaw's planner finds the existing skill via keyword/trigger search (PRD §11.5) and reuses it. Token cost should drop ~30% on the second run because:

1. Tool-schema fingerprint is hot → `Tools: still active` short-circuit fires (PRD §42.1)
2. Cached prompt segments hit on the second turn (PRD §42.2)
3. The `<summary>` protocol replaces full assistant history with 30-char summaries (PRD §42.4)

---

## 4. Inspect what happened

### 4a. Replay the session

```bash
evo replay
```

Picks the most recent JSONL log and pretty-prints:

```
== replay /Users/you/.evoclaw/logs/task-...jsonl (12 records) ==

[TASK] task-20260502T143012.481
  input : list every Cargo.toml under ...
  start : 2026-05-02T14:30:12.481Z

[TURN 0] 1 tool_calls
  summary: scanning workspace
  ✓ list_dir args={"path":"."} → d crates ... | d crates-ext ...
  usage: in=620 cached=400 (65% hit) out=44

[TURN 1] 1 tool_calls
  summary: read 3 manifests
  ✓ read_file args={"path":"crates/evo-core/Cargo.toml"} → ...

[END] COMPLETED @ 2026-05-02T14:30:39.910Z
```

### 4b. Cost / token stats

```bash
evo doctor-of tokens
```

Output:

```
== evo doctor tokens ==
metric              7d         30d
events               4           4
input_tokens     2,480       2,480
cached_tokens    1,612       1,612
output_tokens      244         244
cache_hit         65.00%      65.00%
usd_total       0.0078$     0.0078$

budget: per_task ≤ $0.50, per_day ≤ $5.00 (soft) / $20.00 (hard), per_month ≤ $100
```

A few cents per task on DeepSeek is normal. If you blow past the per-task cap, EvoClaw stops the loop and reports a `Budget(...)` error (PRD §35).

### 4c. Closure audit

```bash
evo doctor-of closure
```

Verifies every session JSONL has a `TaskRecord`, ≥1 `TurnRecord`, and an `EndRecord` (PRD §39 row 1 / 4).

### 4d. Memory grep

```bash
evo memory search "cargo manifest"
```

Returns matching L1/L2/L3 records. Memory is plain text + grep — no vector DB by design. Vectors cost prompt tokens at retrieval time; substring matching against typed memory layers does not.

---

## 5. Watch the skill tree grow

After 5–10 different tasks:

```bash
evo skill tree
```

Output:

```
== skill tree (8 nodes, 2 active) ==

[Diagnostic]
  skill-...     ACTIVE     score=0.86  diagnose ssh hang   (triggers: ssh, diagnose)
  skill-...     CANDIDATE  score=0.62  docker healthcheck  (triggers: docker)

[Sop]
  skill-...     DRAFT      score=0.50  enumerate cargo manifests (triggers: cargo, manifest)
  ...
```

States transition automatically (PRD §32):

- **DRAFT** → **CANDIDATE** after sandbox pass
- **CANDIDATE** → **ACTIVE** after ≥3 successful real uses + score ≥ 0.7
- **ACTIVE** → **DEGRADED** if score drops below 0.7
- **DEGRADED** → **DEPRECATED** after 5 consecutive failures or score < 0.3

Only **ACTIVE** skills are auto-loaded into the planner; **CANDIDATE** can be opted into.

---

## 6. Optional: WebChat via the local Gateway

```bash
evo gateway --bind 127.0.0.1:7878 --token mychat
```

Open <http://127.0.0.1:7878> in a browser. The page asks for a **Bearer token** — type `mychat` (or whatever you passed). Send a message; it goes through the same `ConversationRuntime` as the CLI, with full reflection / cost / memory.

Stop with `Ctrl-C`. The Gateway is local-only (`127.0.0.1`); cookies / API keys never leave your machine.

---

## 7. Where to go next

- **[Usage reference](usage.md)** — all CLI commands, all config knobs, all environment variables
- **[Architecture overview](architecture.md)** — what's in each crate, how the agent loop works
- **[Contributing](contributing.md)** — fix a bug, add a tool, change a prompt
- `prd/prd.md` — the canonical specification (~2,300 lines)
- `prd/plan/development-plan.md` — phase-by-phase task list
- `prd/plan/prompts.md` — every prompt template
