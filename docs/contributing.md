# Contributing to EvoClaw

Thanks for considering a PR. This document explains the rules so we don't have to litigate style in review.

---

## Golden rules

1. **Keep the budgets**. PRD §45.2 caps every crate at a specific LOC count. `./scripts/check.sh` enforces it. If your change pushes a crate over, either trim or split into a sub-crate.
2. **Keep the system prompt at exactly 6 lines** (PRD §44.1, PROMPTS §1). The CI gate fails otherwise.
3. **Keep the tool count ≤ 10** (PRD §43). Adding a new tool requires a PRD §43 update + retiring a slot OR justifying the bump.
4. **Tests stay green**. `cargo test --workspace --all-targets` must pass. If you change behaviour, add a test before the implementation.
5. **Clippy stays green** with `-D warnings`. No new warnings.
6. **No new dependencies without justification**. The whole repo has ~25 direct deps; we treat each as a liability.
7. **No silent failures**. If something can fail, it returns `Result<_, _>`. PRD §34's failure-recovery matrix tells you which row your error class belongs to.

---

## Local setup

See **[Installation](installation.md)**. Then:

```bash
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets -- -D warnings
./scripts/check.sh
```

If all four green, you're ready.

---

## Making a change

### 1. Decide what spec section governs it

| Change kind | Read first | Then update if needed |
|-------------|------------|------------------------|
| New feature | `prd/prd.md` corresponding section | the section |
| New tool | PRD §43 | §43 + tool inventory in `docs/usage.md` |
| New prompt | `prd/plan/prompts.md` numbered template | the template + cite from code |
| Fix a bug | failure recovery matrix PRD §34 | row → state machine in PRD §31 |
| Refactor | `prd/architecture.html` | that diagram if structure changes |

### 2. Write the test first

We follow TDD when behaviour changes. Patterns we use:

- **Sync unit test** lives in `mod tests {}` at the bottom of the file under test.
- **Async test** uses `#[tokio::test]`.
- **Integration test** lives under `crates/<crate>/tests/`.
- **Mock provider** is `evo-mock-provider`; never hit the network in tests.
- **Temp paths** use `unique_*` helpers (atomic counter + pid) to survive parallel runs.

### 3. Write the smallest implementation that passes the test

PRD discipline: don't preemptively generalise. If the spec didn't ask for a feature, don't add it.

### 4. Update docs

- Touched a CLI command? → `docs/usage.md` and `docs/zh/usage.md`
- Touched the loop? → `docs/architecture.md` + diagrams
- Touched a prompt? → `prd/plan/prompts.md`
- Added a tool? → `docs/usage.md` tool table + PRD §43

### 5. Run the gates

```bash
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
./scripts/check.sh
```

If any gate fails, fix before opening the PR. CI will run all three on push.

### 6. Open the PR

Title: `<area>: <verb> <object>` (e.g. `evo-tools: add list_dir excludes`).
Body should answer:

- **Why**: link the PRD section
- **What**: 3-bullet diff summary
- **Test**: which test exercises it; how you ran it manually
- **Risk**: budgets, regressions, anything spooky

---

## Code style

We don't keep a long Rust style guide; instead:

- `cargo fmt` (defaults; no `rustfmt.toml`)
- Prefer `?` over `unwrap()` outside tests
- Prefer plain functions over traits unless ≥2 implementors
- Module size: 100–400 lines; if you see >500, consider splitting
- File-per-tool inside `evo-tools` is acceptable but not required when tools are < 80 lines each
- Public API needs `///` doc comments; private helpers are documented when non-obvious

---

## Adding a new tool — worked example

1. Make sure tool count stays ≤ 10. If at 10, retire one first via PRD §43 update.
2. Add a struct + `impl Tool` block in `crates/evo-tools/src/lib.rs`:

   ```rust
   pub struct MyTool;

   #[async_trait]
   impl Tool for MyTool {
       fn name(&self) -> &'static str { "my_tool" }
       fn description(&self) -> &'static str { "Description ≤ 80 chars." }
       fn permission(&self) -> Permission { Permission::P0 }
       fn schema(&self) -> Value { json!({/* JSON Schema */}) }
       async fn run(&self, ctx: &ToolContext, args: Value) -> Result<String, ToolError> {
           // ...
       }
   }
   inventory::submit!(ToolFactory { build: || Box::new(MyTool) });
   ```

3. Write at least one `#[tokio::test]` covering happy path + one error path.
4. Update `docs/usage.md` tool inventory table.
5. Update `prd/prd.md` §43 inventory.
6. Run `./scripts/check.sh` — the tool count gate confirms ≤ 10.

---

## Adding a new provider — worked example

1. Implement `Provider` for your new struct in `crates/evo-providers/src/yourmod.rs`.
2. Add `pub mod yourmod;` and re-export in `lib.rs`.
3. Wire it into `evo-cli/src/main.rs::run` with prefix-based routing.
4. Test with `evo-mock-provider` patterns.
5. Document in `docs/installation.md` "switch provider" section.

---

## What NOT to do

- Don't add features not in the PRD.
- Don't bypass the prompts file with inline strings in code.
- Don't `unsafe { ... }` without a comment explaining the invariant.
- Don't make `evo-core` depend on `evo-cli` (layering rule).
- Don't introduce a new dep when an existing dep can do the job.
- Don't commit `target/`, `~/.evoclaw/`, or your own API keys.

---

## Releasing & version bumps

**Every meaningful change ships with a global version bump.** The version is
recorded in **two `version` files** that must hold the **identical** value:

- `EvoClaw/version` (this repo)
- `EvoClawSite/version` (the site repo at <https://github.com/develolin/EvoClawSite>)

A version bump must update **all** of the following at once:

1. `EvoClaw/version` — single line, `vX.Y.Z\n`
2. `EvoClawSite/version` — same value
3. `EvoClaw/Cargo.toml` `[workspace.package].version` — `"X.Y.Z"` (no `v` prefix; SemVer)
4. `EvoClaw/README.md` and `EvoClaw/docs/zh/README.md`:
   - The banner-art line `local-first · self-evolving · vX.Y.Z`
   - The "Versioning" section (`Both currently read **`vX.Y.Z`**`)
   - The closing prose ("ships in vX.Y.Z")
5. `EvoClawSite/index.html` and `EvoClawSite/zh.html`:
   - The `softwareVersion` field in the JSON-LD `SoftwareApplication` block
   - The Resources line (`Version: vX.Y.Z` / `版本：vX.Y.Z`)
   - The FAQ JSON-LD answer about production readiness

Use SemVer:

| Bump | When |
|------|------|
| `+1` patch (`v0.1.9 → v0.1.10`) | Bug fix, doc rewrite, CI workflow change, license addition. |
| `+1` minor (`v0.1.x → v0.2.0`) | New CLI subcommand, new built-in tool, new built-in provider/MCP catalog entry, schema-compatible additions. |
| `+1` major (`v0.x.y → v1.0.0`) | Breaking change to `~/.evoclaw/` layout, JSONL record schema, public Rust API, or CLI flags. |

Quick sanity check before opening the PR:

```bash
diff EvoClaw/version EvoClawSite/version && echo MATCH
grep '^version' EvoClaw/Cargo.toml          # cargo SemVer (no v prefix)
grep -rE 'v[0-9]+\.[0-9]+\.[0-9]+' EvoClaw/README.md EvoClaw/docs/zh/README.md \
  EvoClawSite/index.html EvoClawSite/zh.html | grep -v "v0\.X\.Y" | sort -u
```

`./scripts/check.sh` runs a `version sync` gate that asserts the
`EvoClaw/version` file matches `Cargo.toml` `[workspace.package].version`
(it fails the build on drift). The two `version` files (`EvoClaw/version`
vs `EvoClawSite/version`) live in separate repos, so the cross-repo
match is still a process rule — run the sanity block above before
opening the PR.

---

## Reporting issues

Open a GitHub issue with:

- `cargo --version`, OS, shell
- `evo doctor` output (redact your `api_key` line)
- minimal repro (one `evo run "..."` command + the JSONL session log if available)

For security-sensitive findings, email the maintainers privately rather than opening a public issue.

---

## License

By contributing you agree your work is licensed under the project's MIT license. See `LICENSE` at the repo root.
