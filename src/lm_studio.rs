use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Configuration for code generation requests
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationConfig {
    pub temperature: f32,
    pub max_tokens: u32,
    pub top_p: f32,
    pub stop_sequences: Vec<String>,
    pub seed: Option<u64>,
}

impl Default for GenerationConfig {
    fn default() -> Self {
        Self {
            temperature: 0.3,
            max_tokens: 1024,
            top_p: 1.0,
            stop_sequences: vec![],
            seed: None,
        }
    }
}

/// Response from the inference provider
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelResponse {
    pub content: String,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub finish_reason: Option<String>,
    pub latency_ms: u64,
}

/// Trait for swapping inference backends
#[async_trait]
pub trait InferenceProvider: Send + Sync {
    async fn generate(&self, prompt: &str, config: &GenerationConfig) -> Result<ModelResponse>;
    async fn health(&self) -> Result<bool>;
    fn name(&self) -> &str;
}

/// LM Studio API client implementing the InferenceProvider trait
pub struct LmStudio {
    client: Client,
    base_url: String,
    model: String,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    temperature: f32,
    max_tokens: u32,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: MessageContent,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MessageContent {
    content: String,
}

#[derive(Debug, Deserialize)]
struct Usage {
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
}

impl LmStudio {
    pub fn new(base_url: &str, model: &str) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            base_url: base_url.to_string(),
            model: model.to_string(),
        }
    }

    /// Generate code for a given prompt (convenience wrapper over InferenceProvider)
    pub async fn generate_code(
        &self,
        prompt: &str,
        signature: &str,
        context: &str,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<String> {
        let full_prompt = build_code_prompt(prompt, signature, context);
        let config = GenerationConfig {
            temperature,
            max_tokens,
            ..Default::default()
        };
        let response = self.generate(&full_prompt, &config).await?;
        Ok(extract_code(&response.content))
    }

    /// Check if LM Studio is available
    pub async fn health_check(&self) -> Result<bool> {
        self.health().await
    }
}

#[async_trait]
impl InferenceProvider for LmStudio {
    async fn generate(&self, prompt: &str, config: &GenerationConfig) -> Result<ModelResponse> {
        let system_prompt = r#"You are an expert Rust programmer. Generate ONLY valid Rust code.
Do not include markdown code blocks (no ```rust or ```).
Do not include explanations or comments unless part of the code.
The code must compile as-is when placed in a Rust file."#;

        let request = ChatRequest {
            model: self.model.clone(),
            messages: vec![
                Message {
                    role: "system".to_string(),
                    content: system_prompt.to_string(),
                },
                Message {
                    role: "user".to_string(),
                    content: prompt.to_string(),
                },
            ],
            temperature: config.temperature,
            max_tokens: config.max_tokens,
            stream: false,
            top_p: if config.top_p < 1.0 { Some(config.top_p) } else { None },
            seed: config.seed,
        };

        let url = format!("{}/chat/completions", self.base_url);
        let start = Instant::now();

        let response = self.client
            .post(&url)
            .json(&request)
            .send()
            .await
            .context("Failed to send request to LM Studio")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("LM Studio returned error {}: {}", status, body);
        }

        let chat_response: ChatResponse = response
            .json()
            .await
            .context("Failed to parse LM Studio response")?;

        let latency_ms = start.elapsed().as_millis() as u64;

        let content = chat_response
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .unwrap_or_default();

        let finish_reason = chat_response
            .choices
            .first()
            .and_then(|c| c.finish_reason.clone());

        let (input_tokens, output_tokens) = chat_response.usage
            .map(|u| (u.prompt_tokens, u.completion_tokens))
            .unwrap_or((None, None));

        Ok(ModelResponse {
            content,
            input_tokens,
            output_tokens,
            finish_reason,
            latency_ms,
        })
    }

    async fn health(&self) -> Result<bool> {
        let url = format!("{}/models", self.base_url);
        match self.client.get(&url).send().await {
            Ok(response) => Ok(response.status().is_success()),
            Err(_) => Ok(false),
        }
    }

    fn name(&self) -> &str {
        &self.model
    }
}

/// Build the full prompt for code generation
fn build_code_prompt(prompt: &str, signature: &str, context: &str) -> String {
    if context.is_empty() {
        format!(
            "{}\n\nImplement this function:\n{}\n\nProvide the complete function implementation.",
            prompt, signature
        )
    } else {
        format!(
            "{}\n\nContext (already provided, do not repeat):\n{}\n\nImplement this:\n{}\n\nProvide ONLY the implementation, not the context.",
            prompt, context, signature
        )
    }
}

/// Extract code from LLM response (handles markdown code blocks)
fn extract_code(response: &str) -> String {
    let response = response.trim();

    if response.contains("```") {
        let mut in_code_block = false;
        let mut code_lines = Vec::new();

        for line in response.lines() {
            if line.trim().starts_with("```") {
                if in_code_block {
                    break;
                } else {
                    in_code_block = true;
                    continue;
                }
            }
            if in_code_block {
                code_lines.push(line);
            }
        }

        if !code_lines.is_empty() {
            return code_lines.join("\n");
        }
    }

    response.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_code_plain() {
        let input = "fn add(a: i32, b: i32) -> i32 { a + b }";
        assert_eq!(extract_code(input), input);
    }

    #[test]
    fn test_extract_code_markdown() {
        let input = "```rust\nfn add(a: i32, b: i32) -> i32 { a + b }\n```";
        assert_eq!(extract_code(input), "fn add(a: i32, b: i32) -> i32 { a + b }");
    }

    #[test]
    fn test_build_prompt_no_context() {
        let p = build_code_prompt("Sum numbers", "pub fn sum(v: &[i32]) -> i32", "");
        assert!(p.contains("Sum numbers"));
        assert!(p.contains("pub fn sum"));
        assert!(!p.contains("Context"));
    }

    #[test]
    fn test_build_prompt_with_context() {
        let p = build_code_prompt("Implement", "pub fn foo()", "struct Bar;");
        assert!(p.contains("Context"));
        assert!(p.contains("struct Bar;"));
    }

    #[test]
    fn test_generation_config_default() {
        let c = GenerationConfig::default();
        assert!((c.temperature - 0.3).abs() < f32::EPSILON);
        assert_eq!(c.max_tokens, 1024);
    }
}
