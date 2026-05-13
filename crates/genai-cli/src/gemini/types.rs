use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Content {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default)]
    pub parts: Vec<Part>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Part {
    Text {
        text: String,
    },
    InlineData {
        #[serde(rename = "inlineData")]
        inline_data: InlineData,
    },
    FunctionCall {
        #[serde(rename = "functionCall")]
        function_call: FunctionCall,
    },
    FunctionResponse {
        #[serde(rename = "functionResponse")]
        function_response: FunctionResponse,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    #[serde(default)]
    pub args: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionResponse {
    pub name: String,
    pub response: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InlineData {
    pub mime_type: String,
    pub data: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_config: Option<ThinkingConfig>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Tool {
    #[serde(rename = "googleSearch")]
    GoogleSearch {},
    #[serde(rename = "urlContext")]
    UrlContext {},
    #[serde(rename = "codeExecution")]
    CodeExecution {},
    #[serde(rename = "functionDeclarations")]
    FunctionDeclarations(Vec<FunctionDeclaration>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDeclaration {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateContentResponse {
    #[serde(default)]
    pub candidates: Vec<Candidate>,
    #[serde(default)]
    pub usage_metadata: Option<UsageMetadata>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub content: Option<Content>,
    #[serde(default)]
    pub finish_reason: Option<FinishReason>,
    #[serde(default)]
    pub finish_message: Option<String>,
}

/// Why the model stopped producing tokens. `Stop` is the only one that
/// usually deserves silence from the CLI; anything else is worth surfacing
/// because it means truncation or refusal. Unknown values from the wire
/// are kept verbatim as `Other(String)` instead of being lost.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FinishReason {
    Stop,
    MaxTokens,
    Safety,
    Recitation,
    MalformedFunctionCall,
    Language,
    Blocklist,
    ProhibitedContent,
    Spii,
    Other,
    Unspecified,
    #[serde(untagged)]
    Unknown(String),
}

impl FinishReason {
    /// True for the only finish that means "the model produced a full
    /// answer." Everything else means truncation, refusal, or worse.
    pub fn is_normal(&self) -> bool {
        matches!(self, FinishReason::Stop)
    }

    /// Stable wire-shaped label suitable for logs and the user-facing
    /// diagnostic. Matches Gemini's SCREAMING_SNAKE_CASE convention.
    pub fn as_str(&self) -> &str {
        match self {
            FinishReason::Stop => "STOP",
            FinishReason::MaxTokens => "MAX_TOKENS",
            FinishReason::Safety => "SAFETY",
            FinishReason::Recitation => "RECITATION",
            FinishReason::MalformedFunctionCall => "MALFORMED_FUNCTION_CALL",
            FinishReason::Language => "LANGUAGE",
            FinishReason::Blocklist => "BLOCKLIST",
            FinishReason::ProhibitedContent => "PROHIBITED_CONTENT",
            FinishReason::Spii => "SPII",
            FinishReason::Other => "OTHER",
            FinishReason::Unspecified => "FINISH_REASON_UNSPECIFIED",
            FinishReason::Unknown(s) => s,
        }
    }
}

impl std::fmt::Display for FinishReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageMetadata {
    #[serde(default)]
    pub prompt_token_count: Option<u32>,
    #[serde(default)]
    pub candidates_token_count: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiErrorEnvelope {
    pub error: ApiError,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiError {
    pub message: String,
}
