#[derive(Debug, Clone, PartialEq)]
pub struct ModelSpec {
    pub id: String,
    pub provider: String,
    pub context_limit: u64,
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub tools: bool,
}

impl ModelSpec {
    pub fn cost(&self, input: u64, output: u64) -> f64 {
        (input as f64 / 1_000_000.0) * self.input_per_mtok
            + (output as f64 / 1_000_000.0) * self.output_per_mtok
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Catalog {
    models: Vec<ModelSpec>,
    fallback: ModelSpec,
}

impl Default for Catalog {
    fn default() -> Self {
        Self::builtin()
    }
}

impl Catalog {
    pub fn builtin() -> Self {
        let m = |id: &str, provider: &str, ctx: u64, inp: f64, out: f64, tools: bool| ModelSpec {
            id: id.to_string(),
            provider: provider.to_string(),
            context_limit: ctx,
            input_per_mtok: inp,
            output_per_mtok: out,
            tools,
        };
        Self {
            models: vec![
                m("claude-sonnet-4-6", "opencode", 1_000_000, 3.0, 15.0, true),
                m("gpt-5-nano", "opencode", 400_000, 0.05, 0.4, true),
                m("glm-4.7", "opencode", 204_800, 0.6, 2.2, true),
                m("deepseek-v4.1-flash", "opencode-go", 1_000_000, 0.15, 0.6, true),
                m("glm-5.2", "opencode-go", 1_000_000, 1.4, 4.4, true),
                m("kimi-k2.7-code", "opencode-go", 262_144, 0.95, 4.0, true),
                m("gpt-4o", "openai", 128_000, 2.5, 10.0, true),
                m("gpt-4o-mini", "openai", 128_000, 0.15, 0.6, true),
                m("o3-mini", "openai", 200_000, 1.1, 4.4, true),
                m("claude-sonnet-4-20250514", "anthropic", 200_000, 3.0, 15.0, true),
                m("claude-3-7-sonnet-20250219", "anthropic", 200_000, 3.0, 15.0, true),
                m("claude-3-5-haiku-20241022", "anthropic", 200_000, 0.8, 4.0, true),
                m("gemini-2.0-flash", "google", 1_000_000, 0.1, 0.4, true),
                m("gemini-2.5-pro", "google", 1_000_000, 1.25, 10.0, true),
                m("grok-2-latest", "xai", 131_072, 2.0, 10.0, true),
                m("grok-beta", "xai", 131_072, 5.0, 15.0, true),
                m("deepseek-chat", "deepseek", 64_000, 0.27, 1.1, true),
                m("llama-3.3-70b-versatile", "groq", 128_000, 0.59, 0.79, true),
                m("qwen2.5-coder:32b", "ollama", 32_768, 0.0, 0.0, true),
                m("llama3.1:8b", "ollama", 131_072, 0.0, 0.0, true),
            ],
            fallback: m("unknown", "unknown", 128_000, 0.0, 0.0, true),
        }
    }

    pub fn with_fallback(mut self, fallback: ModelSpec) -> Self {
        self.fallback = fallback;
        self
    }

    pub fn models(&self) -> &[ModelSpec] {
        &self.models
    }

    pub fn find(&self, id: &str) -> Option<&ModelSpec> {
        let bare = id.rsplit('/').next().unwrap_or(id);
        self.models.iter().find(|m| m.id == id || m.id == bare)
    }

    pub fn spec(&self, id: &str) -> ModelSpec {
        self.find(id).cloned().unwrap_or_else(|| {
            let mut f = self.fallback.clone();
            f.id = id.to_string();
            f
        })
    }

    pub fn context_limit(&self, id: &str) -> u64 {
        self.find(id).map(|m| m.context_limit).unwrap_or(self.fallback.context_limit)
    }

    pub fn cost(&self, id: &str, input: u64, output: u64) -> f64 {
        self.spec(id).cost(input, output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_up_with_and_without_provider_prefix() {
        let c = Catalog::builtin();
        assert_eq!(c.context_limit("gpt-4o"), 128_000);
        assert_eq!(c.context_limit("openai/gpt-4o"), 128_000);
        assert!(c.find("gpt-4o").unwrap().tools);
    }

    #[test]
    fn opencode_gateway_models_are_listed() {
        let c = Catalog::builtin();
        assert_eq!(c.find("deepseek-v4.1-flash").unwrap().provider, "opencode-go");
        assert_eq!(c.find("glm-4.7").unwrap().provider, "opencode");
        assert_eq!(c.context_limit("opencode-go/deepseek-v4.1-flash"), 1_000_000);
    }

    #[test]
    fn unknown_models_use_fallback_not_panic() {
        let c = Catalog::builtin();
        let s = c.spec("totally-made-up");
        assert_eq!(s.id, "totally-made-up");
        assert_eq!(s.context_limit, c.spec("x").context_limit);
        assert_eq!(c.cost("totally-made-up", 1000, 1000), 0.0);
    }

    #[test]
    fn cost_math_is_per_million_tokens() {
        let c = Catalog::builtin();
        let gpt = c.find("gpt-4o").unwrap();
        assert!((gpt.cost(1_000_000, 1_000_000) - 12.5).abs() < 1e-9);
        assert!((gpt.cost(0, 0)).abs() < 1e-12);
    }
}
