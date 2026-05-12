pub mod chat;
pub mod image;
pub mod tts;
pub mod types;

use anyhow::Result;

#[derive(Clone)]
pub struct Client {
    pub(crate) http: reqwest::Client,
    pub(crate) api_key: String,
    pub(crate) base: String,
}

impl Client {
    pub fn new(api_key: impl Into<String>, base: impl Into<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("genai-cli/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            api_key: api_key.into(),
            base: base.into(),
        })
    }
}
