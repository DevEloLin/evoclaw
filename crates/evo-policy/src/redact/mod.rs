//! Secret redaction — re-exports the three focused submodules.

pub(crate) mod classify;
pub(crate) mod redactor;
pub(crate) mod vault;

pub use classify::{classify_secret, fingerprint_of, SecretKind, shannon_entropy};
pub use redactor::{
    is_fully_redacted, scrub_patterns, scrub_patterns_with, scrub_pem_blocks, Redactor,
    RedactionMode,
};
pub use vault::{default_vault_path, Vault, VaultEntry};

#[cfg(test)]
mod tests {
    use super::*;
    use super::classify::is_path_like;
    use super::redactor::strip_assignment;

    #[test]
    fn classify_openai_sk() {
        assert_eq!(
            classify_secret("sk-1234567890ABCDEFghij"),
            SecretKind::OpenAi
        );
    }
    #[test]
    fn classify_anthropic_overrides_openai() {
        assert_eq!(classify_secret("sk-ant-api03-XXXX"), SecretKind::Anthropic);
    }
    #[test]
    fn classify_github_pats() {
        for prefix in ["ghp_", "gho_", "ghu_", "ghs_", "ghr_"] {
            assert_eq!(
                classify_secret(&format!("{prefix}abcdef0123456789")),
                SecretKind::GitHubPat
            );
        }
    }
    #[test]
    fn classify_aws_key_id() {
        assert_eq!(
            classify_secret("AKIAIOSFODNN7EXAMPLE"),
            SecretKind::AwsKeyId
        );
    }
    #[test]
    fn classify_jwt() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjMifQ.signature_part_here";
        assert_eq!(classify_secret(jwt), SecretKind::Jwt);
    }
    #[test]
    fn classify_high_entropy() {
        let s = "K9f4Lq2pZ8xV3wT7yU0nB6mC1aS5dG2eH4jR8bN0";
        assert_eq!(classify_secret(s), SecretKind::GenericHighEntropy);
    }
    #[test]
    fn classify_normal_words_as_unknown() {
        assert_eq!(classify_secret("hello"), SecretKind::Unknown);
        assert_eq!(classify_secret("the quick brown fox"), SecretKind::Unknown);
    }

    #[test]
    fn fingerprint_is_stable_8_chars() {
        let f1 = fingerprint_of("ghp_xxxxx");
        let f2 = fingerprint_of("ghp_xxxxx");
        assert_eq!(f1, f2);
        assert_eq!(f1.len(), 8);
    }

    #[test]
    fn vault_upsert_overwrites_same_name() {
        let mut v = Vault::default();
        v.upsert("gh", "ghp_old00000000000000000");
        v.upsert("gh", "ghp_new00000000000000000");
        assert_eq!(v.entries.len(), 1);
        assert_eq!(v.entries[0].value, "ghp_new00000000000000000");
    }

    #[test]
    fn vault_remove_returns_true_when_present() {
        let mut v = Vault::default();
        v.upsert("gh", "ghp_xxxxxxxxxxxxxxxxxxxx");
        assert!(v.remove("gh"));
        assert!(!v.remove("gh"));
    }

    #[test]
    fn redactor_substitutes_vault_value() {
        let mut v = Vault::default();
        v.upsert("gh", "ghp_secret_value_xxxxx");
        let r = Redactor::from_vault(&v);
        let (out, n) = r.scrub("please use ghp_secret_value_xxxxx now");
        assert_eq!(out, "please use ${SECRET:gh} now");
        assert_eq!(n, 1);
    }

    #[test]
    fn redactor_scrubs_unknown_secret_via_pattern() {
        let r = Redactor::default();
        let (out, n) = r.scrub("token=ghp_unregistered_aaaaaaaaaaaaaaaaaaa end");
        assert!(out.contains("[REDACTED:github_pat:"));
        assert!(!out.contains("ghp_unregistered"));
        assert_eq!(n, 1);
    }

    #[test]
    fn redactor_scrubs_jwt_in_authorization_header() {
        let r = Redactor::default();
        let (out, _) =
            r.scrub("Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ4In0.sig_value_x");
        assert!(out.contains("[REDACTED:jwt:"));
    }

    #[test]
    fn redactor_idempotent() {
        let mut v = Vault::default();
        v.upsert("gh", "ghp_xxxxxxxxxxxxxxxxxxxx");
        let r = Redactor::from_vault(&v);
        let (once, _) = r.scrub("hello ghp_xxxxxxxxxxxxxxxxxxxx world");
        let (twice, hits2) = r.scrub(&once);
        assert_eq!(once, twice);
        assert_eq!(hits2, 0);
    }

    #[test]
    fn longest_value_wins_when_overlapping() {
        let mut v = Vault::default();
        v.upsert("short", "abc");
        v.upsert("long", "abcdef_long_value");
        let r = Redactor::from_vault(&v);
        let (out, _) = r.scrub("see abcdef_long_value here");
        assert!(out.contains("${SECRET:long}"));
        assert!(!out.contains("${SECRET:short}"));
    }

    #[test]
    fn entropy_distinguishes_random_from_words() {
        let words = "the quick brown fox jumps over";
        let random = "K9f4Lq2pZ8xV3wT7yU0nB6mC1aS5";
        assert!(shannon_entropy(random) > shannon_entropy(words));
    }

    #[test]
    fn strip_assignment_handles_quoted_values() {
        assert_eq!(strip_assignment("password=\"hunter2\""), "hunter2");
        assert_eq!(strip_assignment("token:'abc'"), "abc");
    }

    #[test]
    fn empty_vault_only_uses_pattern_matching() {
        let r = Redactor::default();
        let (out, _) = r.scrub("hello world");
        assert_eq!(out, "hello world");
    }

    // ── acp.md regression tests ───────────────────────────────────────────
    //
    // These tests pin the contract from `prd/plan/acp.md`: ordinary user
    // input (CJK prose, file paths, shell commands, mixed-language text)
    // must NOT be classified as a secret, and the model-mode redactor must
    // pass them through verbatim. Real credentials must still be redacted
    // locally.

    #[test]
    fn cjk_prose_is_not_high_entropy() {
        let s = "没有自适应终端大小，横线分割的文本框，没有被两个横线包起来";
        assert_ne!(classify_secret(s), SecretKind::GenericHighEntropy);
        assert_eq!(classify_secret(s), SecretKind::Unknown);
    }

    #[test]
    fn cjk_prose_survives_model_redactor() {
        let r = Redactor::default();
        let s = "没有自适应终端大小，横线分割的文本框，没有被两个横线包起来";
        let (out, n) = r.redact_for_model(s);
        assert_eq!(out, s);
        assert_eq!(n, 0);
    }

    #[test]
    fn english_question_survives_model_redactor() {
        let r = Redactor::default();
        let s = "如何确定当前是哪个账户";
        let (out, n) = r.redact_for_model(s);
        assert_eq!(out, s);
        assert_eq!(n, 0);
    }

    #[test]
    fn mixed_input_with_emoji_survives_model_redactor() {
        let r = Redactor::default();
        let s = "中文 English emoji 🚀 mixed input should wrap correctly.";
        let (out, n) = r.redact_for_model(s);
        assert_eq!(out, s);
        assert_eq!(n, 0);
    }

    #[test]
    fn unix_path_is_not_redacted() {
        let r = Redactor::default();
        let s = "/Users/wei.li/devops/gptcli/agent/EvoClaw";
        let (out, n) = r.redact_for_model(s);
        assert_eq!(out, s);
        assert_eq!(n, 0);
        // Even strict log mode should not redact a plain path.
        let (out_log, n_log) = r.redact_for_log(s);
        assert_eq!(out_log, s);
        assert_eq!(n_log, 0);
    }

    #[test]
    fn shell_command_is_not_redacted() {
        let r = Redactor::default();
        let s = "cargo clippy --workspace --all-targets -- -D warnings";
        let (out, n) = r.redact_for_model(s);
        assert_eq!(out, s);
        assert_eq!(n, 0);
    }

    #[test]
    fn real_openai_key_is_still_redacted_in_model_mode() {
        let r = Redactor::default();
        let s = "我的 API key 是 sk-1234567890abcdefghijklmnopqrstuvwxyz，帮我检查配置";
        let (out, n) = r.redact_for_model(s);
        assert!(
            !out.contains("sk-1234567890abcdefghijklmnopqrstuvwxyz"),
            "raw key leaked: {out}"
        );
        assert!(out.contains("[REDACTED:openai_key:"), "no marker: {out}");
        // Surrounding prose must be preserved verbatim.
        assert!(out.contains("我的 API key 是 "));
        assert!(out.contains("，帮我检查配置"));
        assert_eq!(n, 1);
    }

    #[test]
    fn github_pat_is_still_redacted_in_model_mode() {
        let r = Redactor::default();
        let s = "use ghp_1234567890abcdefghijklmnopqrstuvwxyz here";
        let (out, n) = r.redact_for_model(s);
        assert!(out.contains("[REDACTED:github_pat:"));
        assert!(out.starts_with("use "));
        assert!(out.ends_with(" here"));
        assert_eq!(n, 1);
    }

    #[test]
    fn jwt_is_still_redacted_in_model_mode() {
        let r = Redactor::default();
        let s =
            "token eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjMifQ.signature_part_here end";
        let (out, _) = r.redact_for_model(s);
        assert!(out.contains("[REDACTED:jwt:"));
        assert!(out.starts_with("token "));
        assert!(out.ends_with(" end"));
    }

    #[test]
    fn pem_block_is_redacted_in_both_modes() {
        let r = Redactor::default();
        let s = "before -----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY----- after";
        let (model_out, _) = r.redact_for_model(s);
        let (log_out, _) = r.redact_for_log(s);
        assert!(model_out.contains("[REDACTED:pem_private_key]"));
        assert!(log_out.contains("[REDACTED:pem_private_key]"));
        assert!(!model_out.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn high_entropy_token_redacted_in_log_but_not_in_model() {
        let r = Redactor::default();
        let s = "ref K9f4Lq2pZ8xV3wT7yU0nB6mC1aS5dG2eH4jR8bN0 ok";
        let (log_out, log_n) = r.redact_for_log(s);
        let (model_out, model_n) = r.redact_for_model(s);
        assert!(log_out.contains("[REDACTED:high_entropy:"));
        assert_eq!(log_n, 1);
        // Model path is conservative — generic high-entropy passes through.
        assert_eq!(model_out, s);
        assert_eq!(model_n, 0);
    }

    #[test]
    fn vault_substitution_runs_in_every_mode() {
        let mut v = Vault::default();
        v.upsert("gh", "ghp_secret_value_xxxxx");
        let r = Redactor::from_vault(&v);
        for mode in [RedactionMode::Log, RedactionMode::Model, RedactionMode::Ui] {
            let (out, n) = r.scrub_with("token=ghp_secret_value_xxxxx end", mode);
            assert!(out.contains("${SECRET:gh}"), "mode {mode:?} -> {out}");
            assert!(n >= 1);
        }
    }

    #[test]
    fn fully_redacted_detector_recognises_marker_only_payloads() {
        assert!(is_fully_redacted("[REDACTED:high_entropy:abcdef00]"));
        assert!(is_fully_redacted("   [REDACTED:openai_key:11223344]   "));
        assert!(is_fully_redacted("${SECRET:gh}"));
        assert!(is_fully_redacted(
            "[REDACTED:high_entropy:aa] [REDACTED:openai_key:bb]"
        ));
    }

    #[test]
    fn fully_redacted_detector_rejects_normal_text() {
        assert!(!is_fully_redacted(""));
        assert!(!is_fully_redacted("   "));
        assert!(!is_fully_redacted("hello world"));
        assert!(!is_fully_redacted(
            "我的 API key 是 [REDACTED:openai_key:abcd] 帮我检查配置"
        ));
        assert!(!is_fully_redacted("ok [REDACTED:openai_key:abcd]"));
    }

    #[test]
    fn path_like_detector_excludes_paths_from_classifier() {
        assert!(is_path_like("/Users/wei.li/devops/gptcli/agent/EvoClaw"));
        assert!(is_path_like("/etc/passwd"));
        assert!(is_path_like("~/Library/Caches"));
        assert!(is_path_like("./scripts/build.sh"));
        assert!(is_path_like("../foo/bar/baz"));
        assert!(is_path_like("C:\\Users\\foo\\bar"));
        assert!(!is_path_like("ghp_xxxxxxxxxxxxxxxx"));
        assert!(!is_path_like("eyJhbGc.eyJzdWI.sig"));
    }

    // ── ask.md regression — identifiers, paths, commands ────────────────
    //
    // Every string in `prd/plan/ask.md`'s "must NOT be redacted" list is
    // pinned here. The CLI screen (`Redactor::redact_for_ui`) and the
    // outbound model path (`Redactor::redact_for_model`) must both leave
    // these untouched.

    fn assert_passthrough_in_ui_and_model(input: &str) {
        let r = Redactor::default();
        let (ui_out, ui_n) = r.redact_for_ui(input);
        let (model_out, model_n) = r.redact_for_model(input);
        assert_eq!(ui_out, input, "UI mode mutated `{input}` → `{ui_out}`");
        assert_eq!(
            model_out, input,
            "Model mode mutated `{input}` → `{model_out}`"
        );
        assert_eq!(ui_n, 0);
        assert_eq!(model_n, 0);
    }

    #[test]
    fn crate_names_pass_through() {
        for s in [
            "evo-cli",
            "evo-core",
            "evo-providers",
            "crates-ext",
            "evo-gateway",
            "evo-policy",
            "evo-tools",
        ] {
            assert_passthrough_in_ui_and_model(s);
        }
    }

    #[test]
    fn rust_identifiers_pass_through() {
        for s in [
            "tokio::spawn",
            "tokio::sync::Semaphore",
            "queued_n",
            "fetch_sub",
            "swap(0)",
            "usize::MAX",
            "rustyline",
            "tui.rs",
            "lib.rs",
            "Cargo.toml",
            "semaphore",
        ] {
            assert_passthrough_in_ui_and_model(s);
        }
    }

    #[test]
    fn slash_commands_pass_through() {
        for s in ["/cancel", "/queue", "/help", "/exit", "/status", "/usage"] {
            assert_passthrough_in_ui_and_model(s);
        }
    }

    #[test]
    fn absolute_paths_pass_through() {
        for s in [
            "/Users/wei.li/devops/gptcli/agent/EvoClaw/crates/evo-cli/src/lib.rs",
            "/Users/wei.li/.evoclaw/config.toml",
            "/tmp/evoclaw/session-20260503T143523.jsonl",
        ] {
            assert_passthrough_in_ui_and_model(s);
        }
    }

    #[test]
    fn path_with_line_number_passes_through() {
        // `path:line` — the `:` is a non-separator so the whole thing is
        // one token; it must still pass through both paths.
        let s = "/Users/wei.li/devops/gptcli/agent/EvoClaw/crates/evo-cli/src/lib.rs:681";
        assert_passthrough_in_ui_and_model(s);
    }

    #[test]
    fn relative_path_with_line_number_passes_through() {
        // `evo-cli/src/lib.rs:681` is short (under 32 chars) so it falls
        // out at the length gate, but pin the contract anyway.
        let s = "evo-cli/src/lib.rs:681";
        assert_passthrough_in_ui_and_model(s);
    }

    #[test]
    fn cli_commands_pass_through_each_token() {
        let r = Redactor::default();
        let cmd = "cargo clippy --workspace --all-targets -- -D warnings";
        let (out, n) = r.redact_for_ui(cmd);
        assert_eq!(out, cmd);
        assert_eq!(n, 0);
        let (out_m, n_m) = r.redact_for_model(cmd);
        assert_eq!(out_m, cmd);
        assert_eq!(n_m, 0);
    }

    #[test]
    fn cjk_sentences_from_ask_md_pass_through() {
        for s in [
            "这些输出都没有格式化",
            "没有自适应终端大小",
            "横线分割的文本框没有被两个横线包起来",
            "现在我提问好像 ACP 没有直接发送到",
        ] {
            assert_passthrough_in_ui_and_model(s);
        }
    }

    #[test]
    fn ui_mode_does_not_emit_high_entropy_marker_on_real_assistant_output() {
        // Simulated assistant reply mixing prose, code names, paths,
        // and a bullet list. None of it is a credential, none of it
        // should produce `[REDACTED:high_entropy:...]`.
        let r = Redactor::default();
        let s = "这次修改主要涉及 evo-cli、evo-core、evo-providers。\n\
                 路径 /Users/wei.li/devops/gptcli/agent/EvoClaw/crates/evo-cli/src/lib.rs:681 \
                 调用 tokio::spawn + Semaphore::new(1)。\n\
                 - 风险 1: queued_n.fetch_sub 之后 worker 仍然继续。\n\
                 - 风险 2: /cancel 只清空队列。\n\
                 cargo clippy --workspace --all-targets -- -D warnings 通过。";
        let (out, n) = r.redact_for_ui(s);
        assert_eq!(out, s, "ui out diverged: {out}");
        assert_eq!(n, 0);
        assert!(
            !out.contains("[REDACTED:high_entropy"),
            "ui output contained high_entropy marker"
        );
    }

    #[test]
    fn ui_mode_still_redacts_real_keys() {
        let r = Redactor::default();
        let s = "我的 key 是 sk-1234567890abcdefghijklmnopqrstuvwxyz，帮我检查配置";
        let (out, _) = r.redact_for_ui(s);
        assert!(!out.contains("sk-1234567890abcdefghijklmnopqrstuvwxyz"));
        assert!(out.contains("[REDACTED:openai_key:"));
        assert!(out.contains("我的 key 是"));
        assert!(out.contains("，帮我检查配置"));
    }

    #[test]
    fn ui_mode_still_redacts_pem_blocks() {
        let r = Redactor::default();
        let s = "前 -----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY----- 后";
        let (out, _) = r.redact_for_ui(s);
        assert!(out.contains("[REDACTED:pem_private_key]"));
        assert!(!out.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn ui_mode_still_redacts_jwt_and_github_pat() {
        let r = Redactor::default();
        let s = "use ghp_1234567890abcdefghijklmnopqrstuvwxyz and \
                 eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjMifQ.signature_part_here";
        let (out, _) = r.redact_for_ui(s);
        assert!(out.contains("[REDACTED:github_pat:"));
        assert!(out.contains("[REDACTED:jwt:"));
        assert!(!out.contains("ghp_1234567890abcdefghijklmnopqrstuvwxyz"));
    }

    #[tokio::test]
    async fn vault_save_then_load_round_trip() {
        let dir = std::env::temp_dir().join(format!(
            "evo-vault-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("vault.json");
        let mut v = Vault::default();
        v.upsert("gh", "ghp_xxxxxxxxxxxxxxxxxxxx");
        v.save(&path).await.unwrap();
        let back = Vault::load(&path).await.unwrap();
        assert_eq!(back.entries.len(), 1);
        assert_eq!(back.entries[0].name, "gh");
        assert_eq!(back.entries[0].value, "ghp_xxxxxxxxxxxxxxxxxxxx");
        // Cleanup ignored on error.
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
