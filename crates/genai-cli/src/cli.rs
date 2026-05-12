use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "genai", version, about = "Gemini CLI: chat, image, audio, music")]
pub struct Cli {
    #[arg(short = 'm', long, help = "Model id or alias")]
    pub model: Option<String>,

    #[arg(short = 'r', long, help = "Role preset name")]
    pub role: Option<String>,

    #[arg(short = 's', long, help = "Session name (create or resume)")]
    pub session: Option<String>,

    #[arg(short = 'f', long, help = "Attach input file(s)")]
    pub file: Vec<String>,

    #[arg(short = 'o', long, help = "Output file path; '-' for stdout")]
    pub output: Option<String>,

    #[arg(long, help = "Disable streaming output")]
    pub no_stream: bool,

    #[command(subcommand)]
    pub command: Option<Command>,

    #[arg(trailing_var_arg = true, help = "Prompt text")]
    pub prompt: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List or inspect models
    Models {
        #[command(subcommand)]
        sub: ModelsCmd,
    },
    /// Manage chat sessions
    Sessions {
        #[command(subcommand)]
        sub: SessionsCmd,
    },
    /// Garbage-collect orphaned attachment blobs
    Gc,
}

#[derive(Debug, Subcommand)]
pub enum ModelsCmd {
    /// List bundled and user-overlay models
    List,
}

#[derive(Debug, Subcommand)]
pub enum SessionsCmd {
    /// List all sessions
    List,
    /// Delete a session
    Delete { name: String },
    /// Export session as JSONL (stdout if path omitted or "-")
    Export {
        name: String,
        #[arg(short, long)]
        output: Option<String>,
    },
}

impl Cli {
    pub fn prompt_text(&self) -> Option<String> {
        if self.prompt.is_empty() {
            None
        } else {
            Some(self.prompt.join(" "))
        }
    }
}
