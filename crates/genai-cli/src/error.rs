use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("config error: {0}")]
    Config(String),

    #[error("api error: {0}")]
    Api(String),

    #[error("session error: {0}")]
    Session(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("db: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("http: {0}")]
    Http(#[from] reqwest::Error),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
