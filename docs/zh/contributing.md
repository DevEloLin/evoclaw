# 贡献指南

感谢愿意提 PR。本文是规则约定，避免在 review 时再扯皮风格。

---

## 黄金规则

1. **守预算**。PRD §45.2 给每个 crate 定了 LOC 上限。`./scripts/check.sh` 会强制检查。超了就要么瘦身、要么拆子 crate。
2. **system prompt 严格 6 行**（PRD §44.1, PROMPTS §1）。CI 门会失败否则。
3. **工具数 ≤ 10**（PRD §43）。新增工具必须先改 PRD §43，要么淘汰一个老的，要么把上限调高并附理由。
4. **测试常绿**。`cargo test --workspace --all-targets` 必须过。改行为先加测试。
5. **Clippy `-D warnings` 常绿**。不接受新 warning。
6. **不轻易加依赖**。整个仓库 ~25 个直接依赖，每一个都视为负担。
7. **不静默失败**。能失败的就返回 `Result<_, _>`。PRD §34 失败恢复矩阵告诉你这条错应该归到哪一行。

---

## 本地准备

见 **[安装](installation.md)**。然后：

```bash
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets -- -D warnings
./scripts/check.sh
```

四个全绿就可以开干。

---

## 改动流程

### 1. 找到对应的规格章节

| 改动类型 | 先读 | 视情况更新 |
|---------|------|-----------|
| 新功能 | `prd/prd.md` 对应章节 | 那一节 |
| 新工具 | PRD §43 | §43 + `docs/usage.md` 工具表 |
| 新 prompt | `prd/plan/prompts.md` 编号模板 | 模板 + 代码引用 |
| 修 bug | PRD §34 失败恢复矩阵 | 对应行 → §31 状态机 |
| 重构 | `prd/architecture.html` | 结构变更需更新该图 |

### 2. 先写测试

行为变更走 TDD。我们用的模式：

- **同步单测**：在被测文件底部 `mod tests {}`
- **异步测试**：`#[tokio::test]`
- **集成测试**：`crates/<crate>/tests/`
- **Mock provider**：`evo-mock-provider`，**测试永远不出网**
- **临时路径**：`unique_*` helper（atomic counter + pid）以适配并行测试

### 3. 写最小实现让测试过

PRD 纪律：**别提前泛化**。规格没要求的别加。

### 4. 同步文档

- 改了 CLI 命令？→ `docs/usage.md` + `docs/zh/usage.md`
- 改了 loop？→ `docs/architecture.md` + 图
- 改了 prompt？→ `prd/plan/prompts.md`
- 加了工具？→ `docs/usage.md` 工具表 + PRD §43

### 5. 跑全部门

```bash
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
./scripts/check.sh
```

任一失败先修再开 PR。CI 会在 push 时再跑一次。

### 6. 开 PR

标题：`<area>: <动词> <对象>`（例如 `evo-tools: add list_dir excludes`）。
正文回答：

- **为什么**：链接 PRD 章节
- **改了什么**：3 行 bullet
- **测试**：哪个测试覆盖；如何手动验证
- **风险**：预算、回归、其它需要注意的

---

## 代码风格

不写长篇 Rust 风格指南。规则：

- `cargo fmt`（默认配置；不带 `rustfmt.toml`）
- 测试外尽量 `?` 而不是 `unwrap()`
- 优先平凡函数，trait 留给 ≥2 个实现者
- 模块 100–400 行；超 500 考虑拆分
- `evo-tools` 内一工具一文件可以但非必须，每工具 < 80 行时合一个文件也行
- 公共 API 必须有 `///` 注释；私有 helper 视情况

---

## 加新工具 — 走一遍

1. 工具数保持 ≤ 10。如果已经 10，先改 PRD §43 淘汰一个。
2. 在 `crates/evo-tools/src/lib.rs` 加结构体 + `impl Tool`：

   ```rust
   pub struct MyTool;

   #[async_trait]
   impl Tool for MyTool {
       fn name(&self) -> &'static str { "my_tool" }
       fn description(&self) -> &'static str { "描述 ≤ 80 字符" }
       fn permission(&self) -> Permission { Permission::P0 }
       fn schema(&self) -> Value { json!({/* JSON Schema */}) }
       async fn run(&self, ctx: &ToolContext, args: Value) -> Result<String, ToolError> {
           // ...
       }
   }
   inventory::submit!(ToolFactory { build: || Box::new(MyTool) });
   ```

3. 至少写一个 `#[tokio::test]` happy path + 一个 error path。
4. 更新 `docs/usage.md` 工具表。
5. 更新 PRD §43 工具清单。
6. 跑 `./scripts/check.sh`，工具数门会确认 ≤ 10。

---

## 加新 provider — 走一遍

1. 在 `crates/evo-providers/src/yourmod.rs` 实现 `Provider`。
2. `lib.rs` 加 `pub mod yourmod;` + 重导出。
3. `evo-cli/src/main.rs::run` 接入（参考现有 `OpenAiCompatProvider`）。
4. 用 `evo-mock-provider` 模式测。
5. 在 `docs/installation.md` "切换厂商"段记一笔。

---

## 不要做的事

- 不要加 PRD 没要求的功能。
- 不要在代码里 inline 长 prompt 字符串，全走 PROMPTS 文件。
- 不要 `unsafe { ... }` 不带注释解释 invariant。
- 不要让 `evo-core` 依赖 `evo-cli`（分层规则）。
- 不要新增依赖去做现有依赖能做的事。
- 不要 commit `target/`、`~/.evoclaw/`、自己的 API key。

---

## 报 issue

GitHub issue 包含：

- `cargo --version`、OS、shell
- `evo doctor` 输出（**手动遮掉 api_key 行**）
- 最小复现（一条 `evo run "..."` + 对应 JSONL）

安全敏感问题请私信维护者，不要开公开 issue。

---

## License

提交即同意你的代码以仓库 MIT license 发布。详见根的 `LICENSE`。
