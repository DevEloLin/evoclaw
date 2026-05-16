pub const PROVIDERS: &[ProviderProfile] = &[
    // ---- Top 5 by global popularity (按全球知名度和热度排序) --------
    ProviderProfile {
        id: "openai",
        name: "OpenAI (ChatGPT)",
        base_url: "https://api.openai.com/v1",
        default_model: "gpt-4o-mini",
        key_url: Some("https://platform.openai.com/api-keys"),
        fallback: &["gpt-4o", "gpt-4-turbo"],
        local: false,
    },
    ProviderProfile {
        id: "anthropic",
        name: "Anthropic (Claude)",
        base_url: "https://api.anthropic.com/v1",
        default_model: "claude-3-5-sonnet-20241022",
        key_url: Some("https://console.anthropic.com/settings/keys"),
        fallback: &["claude-3-5-haiku-20241022"],
        local: false,
    },
    ProviderProfile {
        id: "gemini",
        name: "Google Gemini",
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        default_model: "gemini-2.0-flash",
        key_url: Some("https://aistudio.google.com/app/apikey"),
        fallback: &["gemini-1.5-pro", "gemini-1.5-flash"],
        local: false,
    },
    ProviderProfile {
        id: "deepseek",
        name: "DeepSeek (深度求索)",
        base_url: "https://api.deepseek.com/v1",
        default_model: "deepseek-chat",
        key_url: Some("https://platform.deepseek.com/api_keys"),
        fallback: &["deepseek-reasoner"],
        local: false,
    },
    ProviderProfile {
        id: "copilot",
        name: "GitHub Copilot",
        base_url: "https://api.githubcopilot.com",
        default_model: "gpt-4o",
        key_url: None,
        fallback: &["claude-3.5-sonnet"],
        local: false,
    },
    // ---- Other Chinese vendors (其他国内厂商) -----------------------
    ProviderProfile {
        id: "kimi",
        name: "Kimi · Moonshot (月之暗面)",
        base_url: "https://api.moonshot.cn/v1",
        default_model: "kimi-k2-0905-preview",
        key_url: Some("https://platform.moonshot.cn/console/api-keys"),
        fallback: &["moonshot-v1-32k", "moonshot-v1-128k"],
        local: false,
    },
    ProviderProfile {
        id: "qwen",
        name: "Qwen · DashScope (阿里通义千问)",
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        default_model: "qwen-plus",
        key_url: Some("https://bailian.console.aliyun.com/?apiKey=1"),
        fallback: &["qwen-turbo", "qwen-max"],
        local: false,
    },
    ProviderProfile {
        id: "doubao",
        name: "Doubao · Volcengine (字节豆包)",
        base_url: "https://ark.cn-beijing.volces.com/api/v3",
        default_model: "doubao-seed-1-6-250615",
        key_url: Some("https://console.volcengine.com/ark/region:ark+cn-beijing/apiKey"),
        fallback: &["doubao-1-5-pro-32k-250115"],
        local: false,
    },
    ProviderProfile {
        id: "zhipu",
        name: "Zhipu GLM (智谱)",
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        default_model: "glm-4-plus",
        key_url: Some("https://open.bigmodel.cn/usercenter/apikeys"),
        fallback: &["glm-4-flash", "glm-4-air"],
        local: false,
    },
    ProviderProfile {
        id: "baidu",
        name: "Baidu Qianfan (百度千帆 / 文心一言)",
        base_url: "https://qianfan.baidubce.com/v2",
        default_model: "ernie-4.5-turbo-128k",
        key_url: Some("https://console.bce.baidu.com/iam/#/iam/apikey/list"),
        fallback: &["ernie-4.0-8k", "ernie-speed-128k"],
        local: false,
    },
    ProviderProfile {
        id: "minimax",
        name: "MiniMax (海螺 AI)",
        base_url: "https://api.minimax.chat/v1",
        default_model: "MiniMax-Text-01",
        key_url: Some("https://www.minimaxi.com/user-center/basic-information/interface-key"),
        fallback: &["abab6.5s-chat"],
        local: false,
    },
    ProviderProfile {
        id: "stepfun",
        name: "StepFun (阶跃星辰)",
        base_url: "https://api.stepfun.com/v1",
        default_model: "step-2-16k",
        key_url: Some("https://platform.stepfun.com/interface-key"),
        fallback: &["step-1-flash", "step-1-32k"],
        local: false,
    },
    ProviderProfile {
        id: "tencent",
        name: "Tencent Hunyuan (腾讯混元)",
        base_url: "https://api.hunyuan.cloud.tencent.com/v1",
        default_model: "hunyuan-turbos-latest",
        key_url: Some("https://console.cloud.tencent.com/hunyuan/api-key"),
        fallback: &["hunyuan-large", "hunyuan-standard"],
        local: false,
    },
    // ---- Other International vendors (其他国际厂商) -----------------
    ProviderProfile {
        id: "azure",
        name: "Azure AI Foundry / Azure OpenAI",
        base_url: "",
        default_model: "",
        key_url: Some(
            "https://learn.microsoft.com/azure/ai-services/openai/how-to/create-resource",
        ),
        fallback: &[],
        local: false,
    },
    ProviderProfile {
        id: "mistral",
        name: "Mistral AI",
        base_url: "https://api.mistral.ai/v1",
        default_model: "mistral-large-latest",
        key_url: Some("https://console.mistral.ai/api-keys/"),
        fallback: &["mistral-small-latest", "codestral-latest"],
        local: false,
    },
    ProviderProfile {
        id: "groq",
        name: "Groq (LPU inference)",
        base_url: "https://api.groq.com/openai/v1",
        default_model: "llama-3.3-70b-versatile",
        key_url: Some("https://console.groq.com/keys"),
        fallback: &["llama-3.1-8b-instant"],
        local: false,
    },
    ProviderProfile {
        id: "together",
        name: "Together AI",
        base_url: "https://api.together.xyz/v1",
        default_model: "meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo",
        key_url: Some("https://api.together.xyz/settings/api-keys"),
        fallback: &["Qwen/Qwen2.5-72B-Instruct-Turbo"],
        local: false,
    },
    ProviderProfile {
        id: "fireworks",
        name: "Fireworks AI",
        base_url: "https://api.fireworks.ai/inference/v1",
        default_model: "accounts/fireworks/models/llama-v3p3-70b-instruct",
        key_url: Some("https://fireworks.ai/api-keys"),
        fallback: &["accounts/fireworks/models/qwen2p5-72b-instruct"],
        local: false,
    },
    ProviderProfile {
        id: "openrouter",
        name: "OpenRouter (multi-model gateway)",
        base_url: "https://openrouter.ai/api/v1",
        default_model: "openai/gpt-4o-mini",
        key_url: Some("https://openrouter.ai/keys"),
        fallback: &["anthropic/claude-3.5-sonnet", "deepseek/deepseek-chat"],
        local: false,
    },
    // ---- Local / self-hosted (本地 / 自建) ------------------------------
    ProviderProfile {
        id: "ollama",
        name: "Ollama (local)",
        base_url: "http://localhost:11434/v1",
        default_model: "llama3.1",
        key_url: None,
        fallback: &[],
        local: true,
    },
    ProviderProfile {
        id: "vllm",
        name: "vLLM (local)",
        base_url: "http://localhost:8000/v1",
        default_model: "Qwen/Qwen2.5-7B-Instruct",
        key_url: None,
        fallback: &[],
        local: true,
    },
    ProviderProfile {
        id: "llamacpp",
        name: "llama.cpp server (local)",
        base_url: "http://localhost:8080/v1",
        default_model: "default",
        key_url: None,
        fallback: &[],
        local: true,
    },
    // ---- Private / enterprise gateway (私有企业网关) -------------------
    // These entries share the `prompt_custom` flow but show gateway-specific
    // guidance so operators know exactly what URL and token to enter.
    ProviderProfile {
        id: "litellm",
        name: "LiteLLM Gateway (self-hosted proxy)",
        base_url: "",
        default_model: "",
        key_url: Some("https://docs.litellm.ai/docs/proxy/quick_start"),
        fallback: &[],
        local: false,
    },
    ProviderProfile {
        id: "private-gateway",
        name: "Private / Enterprise Gateway (OpenAI-compat, Bearer auth)",
        base_url: "",
        default_model: "",
        key_url: None,
        fallback: &[],
        local: false,
    },
    ProviderProfile {
        id: "custom",
        name: "Custom OpenAI-compatible endpoint",
        base_url: "",
        default_model: "",
        key_url: None,
        fallback: &[],
        local: false,
    },
];

#[derive(Debug, Clone, Copy)]
pub struct ProviderProfile {
    pub id: &'static str,
    pub name: &'static str,
    pub base_url: &'static str,
    pub default_model: &'static str,
    pub key_url: Option<&'static str>,
    pub fallback: &'static [&'static str],
    pub local: bool,
}

pub fn find_provider(id: &str) -> Option<&'static ProviderProfile> {
    PROVIDERS.iter().find(|p| p.id == id)
}

/// Map a provider ID to its corresponding ACP agent ID (if any).
///
/// This allows users to select ACP mode when choosing a provider that has
/// a corresponding ACP agent available.
///
/// Mappings:
/// - anthropic -> claude
/// - openai -> codex
/// - gemini/google -> gemini
/// - copilot -> copilot
/// - qwen -> qwen-code
/// - cursor -> cursor
pub fn provider_to_acp_agent(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        "anthropic" => Some("claude"),
        "openai" => Some("codex"),
        "gemini" | "google" => Some("gemini"),
        "copilot" => Some("copilot"),
        "qwen" => Some("qwen-code"),
        "cursor" => Some("cursor"),
        _ => None,
    }
}
