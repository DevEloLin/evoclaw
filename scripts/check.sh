#!/usr/bin/env bash
# Phase 6.5 + 6.9 — LOC budget enforcer + docs sync check.
# Usage: ./scripts/check.sh [--strict]

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

STRICT=0
for arg in "$@"; do
  case "$arg" in
    --strict) STRICT=1 ;;
  esac
done

fail() { echo "FAIL $*"; exit 1; }
warn() { echo "WARN $*"; [[ "$STRICT" == "1" ]] && exit 1 || true; }
ok()   { echo "OK   $*"; }

# Parallel arrays — bash 3.2 compatible (macOS default).
# Caps reflect v0.3.7 actual scope (post event-driven TUI redesign):
# - evo-cli: event-driven TUI (ui.rs) + REPL + onboard wizard (21 providers) +
#            agent/mcp/secret subcommands + mcp_tools bridge + welcome banner + replay
# - evo-core: full learning loop (skill/memory/reflection/distillation/compression/skill_tree)
# - evo-providers: OpenAI-compat + Anthropic + Copilot + ACP adapter (with /v1/models fetcher)
# - evo-policy: permission ladder + cost engine + Vault/Redactor (PRD §13.4)
# - evo-tools: 7 built-in tools (capped at 10 by PRD §43)
# Hard fail triggers at total > 18040 LOC (16400 target + 10% slack).
crates=(evo-cli   evo-core evo-tools evo-providers evo-policy)
caps=(  7500      4000     1200       2000          1700)
core_total=0
echo "== LOC budget =="
for i in "${!crates[@]}"; do
  crate="${crates[$i]}"
  cap="${caps[$i]}"
  if [[ -d "crates/$crate/src" ]]; then
    loc=$(find "crates/$crate" -name '*.rs' -exec cat {} + | wc -l | tr -d ' ')
  else
    loc=0
  fi
  pct=$(( 100 * loc / cap ))
  if   (( loc > cap * 12 / 10 )); then fail "$crate: $loc / $cap LOC (${pct}%) - over by >20%"
  elif (( loc > cap )); then warn "$crate: $loc / $cap LOC (${pct}%) - over budget"
  else ok "$crate: $loc / $cap LOC (${pct}%)"
  fi
  core_total=$((core_total + loc))
done
echo "core total: $core_total / 16400 LOC ($(( 100 * core_total / 16400 ))%)"
(( core_total > 18040 )) && fail "core total > 16400 by >10%"

echo
echo "== docs sync =="
# prd/ specs and the canonical Mermaid HTML diagrams live in a separate
# private docs repo + the public EvoClawSite (https://github.com/develolin/
# EvoClawSite). The EvoClaw code repo only ships the user docs under docs/
# plus the README and the version file.
required=(
  "README.md"
  "version"
  "docs/installation.md"
  "docs/getting-started.md"
  "docs/usage.md"
  "docs/architecture.md"
  "docs/contributing.md"
  "docs/agents.md"
  "docs/mcp.md"
  "docs/zh/README.md"
  "docs/zh/installation.md"
  "docs/zh/getting-started.md"
  "docs/zh/usage.md"
  "docs/zh/architecture.md"
  "docs/zh/contributing.md"
  "docs/zh/agents.md"
  "docs/zh/mcp.md"
)
for f in "${required[@]}"; do
  [[ -f "$f" ]] && ok "$f present" || fail "missing: $f (deliverable)"
done

echo
echo "== prompt budget =="
# Source of truth for the system prompt is crates/evo-core/src/prompt.rs.
# The prompt is built with format!("...") using \n\ line continuations.
# Count lines from "You are EvoClaw..." to the closing "," line.
prompt_src="crates/evo-core/src/prompt.rs"
if [[ -f "$prompt_src" ]]; then
  sys_lines=$(awk '
    /"You are EvoClaw/ { in_block=1 }
    in_block { count++ }
    in_block && /",[ \t]*$/ { print count; exit }
  ' "$prompt_src")
  sys_lines=${sys_lines:-0}
  if (( sys_lines == 6 )); then
    ok "system prompt body is exactly 6 lines (PRD §44.1)"
  elif (( sys_lines >= 4 && sys_lines <= 8 )); then
    warn "system prompt body has $sys_lines lines (target 6, allowable 4-8)"
  else
    fail "system prompt body has $sys_lines lines, expected 6 (PRD §44.1)"
  fi
else
  warn "$prompt_src not found — skipping prompt budget gate"
fi

echo
echo "== tool count =="
tools=$(grep -cE '^inventory::submit!\(ToolFactory' crates/evo-tools/src/lib.rs || true)
if (( tools <= 10 )); then ok "$tools / 10 tools registered (PRD §43)"; else fail "$tools tools registered, exceeds PRD §43 cap of 10"; fi

echo
echo "== version sync =="
# Source of truth for the release version is the workspace Cargo.toml.
# The user-facing `version` file mirrors it with a leading "v".
cargo_ver=$(awk -F\" '/^version[[:space:]]*=/ {print $2; exit}' Cargo.toml)
file_ver=$(tr -d '[:space:]' < version 2>/dev/null || true)
if [[ -z "$cargo_ver" ]]; then
  fail "could not parse workspace.package.version from Cargo.toml"
elif [[ -z "$file_ver" ]]; then
  fail "version file missing or empty"
elif [[ "$file_ver" != "v$cargo_ver" ]]; then
  fail "version drift: Cargo.toml=$cargo_ver, version file=$file_ver (expected v$cargo_ver)"
else
  ok "version sync: Cargo.toml=$cargo_ver, version=$file_ver"
fi

echo
echo "All gates passed."
