use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::ai::catalog::Catalog;
use crate::config::Config;
use crate::credentials::Credentials;
use crate::models::ModelEntry;

#[derive(Debug, Default, Serialize, Deserialize)]
struct ModelCache {
    #[serde(default)]
    providers: HashMap<String, CachedModels>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CachedModels {
    #[serde(default)]
    fetched_ms: i64,
    #[serde(default)]
    key_fp: String,
    #[serde(default)]
    models: Vec<String>,
}

static CACHE_LOCK: Mutex<()> = Mutex::new(());

fn cache_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("theta").join("models.json"))
}

fn load_cache() -> ModelCache {
    cache_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_cache(cache: &ModelCache) {
    let Some(path) = cache_path() else { return };
    let Ok(text) = serde_json::to_string_pretty(cache) else { return };
    let _guard = CACHE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, text);
}

fn key_fingerprint(key: Option<&str>) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key.unwrap_or("").hash(&mut h);
    format!("{:016x}", h.finish())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn invalidate(provider: &str) {
    let mut cache = load_cache();
    if cache.providers.remove(provider).is_some() {
        save_cache(&cache);
    }
}

pub fn invalidate_all() {
    if let Some(path) = cache_path() {
        let _ = std::fs::remove_file(path);
    }
}

pub const PROVIDERS: &[(&str, &str, &str)] = &[
    ("opencode", "OpenCode Zen", "OPENCODE_API_KEY"),
    ("opencode-go", "OpenCode Go", "OPENCODE_API_KEY"),
    ("anthropic", "Anthropic", "ANTHROPIC_API_KEY"),
    ("openai", "OpenAI", "OPENAI_API_KEY"),
    ("google", "Google Gemini", "GEMINI_API_KEY"),
    ("xai", "xAI (Grok)", "XAI_API_KEY"),
    ("groq", "Groq", "GROQ_API_KEY"),
    ("deepseek", "DeepSeek", "DEEPSEEK_API_KEY"),
    ("openrouter", "OpenRouter", "OPENROUTER_API_KEY"),
    ("together", "Together AI", "TOGETHER_API_KEY"),
    ("fireworks", "Fireworks AI", "FIREWORKS_API_KEY"),
    ("mistral", "Mistral", "MISTRAL_API_KEY"),
    ("cerebras", "Cerebras", "CEREBRAS_API_KEY"),
    ("perplexity", "Perplexity", "PERPLEXITY_API_KEY"),
    ("ollama", "Ollama (local)", ""),
];

pub fn env_for(provider: &str) -> Option<&'static str> {
    PROVIDERS
        .iter()
        .find(|(id, _, _)| *id == provider)
        .map(|(_, _, env)| *env)
}

fn entry(provider: &str, model: &str, catalog: &Catalog) -> ModelEntry {
    ModelEntry {
        provider_id: provider.to_string(),
        model_id: model.to_string(),
        label: format!("{provider}/{model}"),
        context_limit: catalog.find(model).map(|s| s.context_limit),
    }
}

pub fn catalog_entries() -> Vec<ModelEntry> {
    catalog_entries_for(None)
}

pub fn catalog_entries_for(provider: Option<&str>) -> Vec<ModelEntry> {
    let cat = Catalog::builtin();
    cat.models()
        .iter()
        .filter(|s| provider.map(|p| s.provider == p).unwrap_or(true))
        .map(|s| ModelEntry {
            provider_id: s.provider.clone(),
            model_id: s.id.clone(),
            label: format!("{}/{}", s.provider, s.id),
            context_limit: Some(s.context_limit),
        })
        .collect()
}

pub fn providers_to_query(
    cfg: &Config,
    creds: &Credentials,
) -> Vec<(String, String, Option<String>)> {
    let active = cfg.ai.provider.to_ascii_lowercase();
    let mut out = Vec::new();
    if active == "custom" && !cfg.ai.base_url.trim().is_empty() {
        out.push((
            "custom".to_string(),
            cfg.ai.base_url.clone(),
            creds.resolve("custom", &cfg.ai.api_key_env),
        ));
    }
    for (id, _label, env) in PROVIDERS {
        let id = id.to_ascii_lowercase();
        let is_active = id == active;
        let stored = creds
            .api_keys
            .get(&id)
            .filter(|k| !k.trim().is_empty())
            .cloned();
        let key = match stored {
            Some(k) => Some(k),
            None if is_active => creds.resolve(&id, env),
            None => None,
        };
        let is_local_ollama = id == "ollama" && is_active;
        if key.is_none() && !is_local_ollama {
            continue;
        }
        let base = if is_active { cfg.ai.base_url.clone() } else { String::new() };
        out.push((id, base, key));
    }
    out
}

async fn models_for(
    id: &str,
    base: &str,
    key: Option<String>,
    cache: &mut ModelCache,
    dirty: &mut bool,
) -> Vec<String> {
    let fp = key_fingerprint(key.as_deref());
    let cached = cache.providers.get(id).cloned();
    if let Some(c) = &cached {
        if c.key_fp == fp && !c.models.is_empty() {
            return c.models.clone();
        }
    }
    let Ok(provider) = crate::ai::provider_for(id, base, key) else {
        return cached.map(|c| c.models).unwrap_or_default();
    };
    match provider.list_models().await {
        Ok(models) if !models.is_empty() => {
            cache.providers.insert(
                id.to_string(),
                CachedModels { fetched_ms: now_ms(), key_fp: fp, models: models.clone() },
            );
            *dirty = true;
            models
        }
        _ => cached.map(|c| c.models).unwrap_or_default(),
    }
}

pub async fn discover_models(cfg: &Config) -> Vec<ModelEntry> {
    let catalog = Catalog::builtin();
    let creds = Credentials::load();
    let mut cache = load_cache();
    let mut dirty = false;

    let mut entries: Vec<ModelEntry> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let push = |provider: &str,
                model: &str,
                entries: &mut Vec<ModelEntry>,
                seen: &mut HashSet<(String, String)>| {
        let model = model.trim();
        if model.is_empty() {
            return;
        }
        if seen.insert((provider.to_string(), model.to_string())) {
            entries.push(entry(provider, model, &catalog));
        }
    };

    for (id, base, key) in providers_to_query(cfg, &creds) {
        let mut models = models_for(&id, &base, key, &mut cache, &mut dirty).await;
        if models.is_empty() {
            models = catalog
                .models()
                .iter()
                .filter(|m| m.provider == id)
                .map(|m| m.id.clone())
                .collect();
        }
        for m in models {
            push(&id, &m, &mut entries, &mut seen);
        }
    }

    if dirty {
        save_cache(&cache);
    }

    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn creds_with(pairs: &[(&str, &str)]) -> Credentials {
        Credentials {
            api_keys: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<HashMap<_, _>>(),
        }
    }

    #[test]
    fn provider_table_leads_with_opencode() {
        assert_eq!(PROVIDERS[0].0, "opencode");
        assert_eq!(PROVIDERS[1].0, "opencode-go");
        assert_eq!(env_for("opencode-go"), Some("OPENCODE_API_KEY"));
        assert_eq!(env_for("nope"), None);
    }

    #[test]
    fn catalog_entries_include_opencode_gateways() {
        let e = catalog_entries();
        assert!(e
            .iter()
            .any(|m| m.provider_id == "opencode-go" && m.model_id == "deepseek-v4.1-flash"));
        assert!(e.iter().all(|m| m.label.contains('/')));
    }

    #[test]
    fn only_configured_providers_are_queried() {
        let mut cfg = Config::default();
        cfg.ai.provider = "custom".into();

        let q = providers_to_query(&cfg, &creds_with(&[("anthropic", "sk-ant")]));
        let ids: Vec<&str> = q.iter().map(|(id, _, _)| id.as_str()).collect();
        assert_eq!(ids, vec!["anthropic"], "only the configured provider");
        assert!(!ids.contains(&"ollama"), "unconfigured Ollama is hidden");
        assert!(!ids.contains(&"openai"));

        let q = providers_to_query(
            &cfg,
            &creds_with(&[("opencode", "k"), ("opencode-go", "k2")]),
        );
        let ids: Vec<&str> = q.iter().map(|(id, _, _)| id.as_str()).collect();
        assert!(ids.contains(&"opencode"));
        assert!(ids.contains(&"opencode-go"));

        cfg.ai.provider = "ollama".into();
        let q = providers_to_query(&cfg, &creds_with(&[]));
        let ids: Vec<&str> = q.iter().map(|(id, _, _)| id.as_str()).collect();
        assert_eq!(ids, vec!["ollama"]);
    }

    #[test]
    fn catalog_can_be_filtered_to_one_provider() {
        let go = catalog_entries_for(Some("opencode-go"));
        assert!(!go.is_empty());
        assert!(go.iter().all(|m| m.provider_id == "opencode-go"));
        assert!(catalog_entries_for(Some("nope")).is_empty());
    }

    #[test]
    fn key_fingerprint_tracks_the_key() {
        assert_eq!(key_fingerprint(Some("a")), key_fingerprint(Some("a")));
        assert_ne!(key_fingerprint(Some("a")), key_fingerprint(Some("b")));
        assert_eq!(key_fingerprint(None), key_fingerprint(Some("")));
    }

    #[tokio::test]
    async fn cached_models_are_reused_without_a_refetch() {
        let key = Some("sk-test".to_string());
        let mut cache = ModelCache::default();
        cache.providers.insert(
            "acme".into(),
            CachedModels {
                fetched_ms: 1,
                key_fp: key_fingerprint(key.as_deref()),
                models: vec!["m1".into(), "m2".into()],
            },
        );
        let mut dirty = false;
        let models =
            models_for("acme", "http://127.0.0.1:1/v1", key, &mut cache, &mut dirty).await;
        assert_eq!(models, vec!["m1".to_string(), "m2".to_string()]);
        assert!(!dirty, "cache hit must not rewrite the cache");
    }

    #[test]
    fn active_custom_endpoint_is_queried() {
        let mut cfg = Config::default();
        cfg.ai.provider = "custom".into();
        cfg.ai.base_url = "https://gateway.test/v1".into();
        let q = providers_to_query(&cfg, &creds_with(&[("custom", "k")]));
        let custom = q.iter().find(|(id, _, _)| id == "custom").expect("custom queried");
        assert_eq!(custom.1, "https://gateway.test/v1");
        assert_eq!(custom.2.as_deref(), Some("k"));
    }
}
