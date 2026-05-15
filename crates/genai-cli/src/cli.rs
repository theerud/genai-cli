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

    #[arg(short = 'a', long, help = "Image aspect ratio (Imagen only: 1:1, 16:9, 9:16, 4:3, 3:4)")]
    pub aspect: Option<String>,

    #[arg(short = 'n', long, help = "Number of image variants (Imagen only)")]
    pub count: Option<u32>,

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
    /// First-run setup wizard
    Init {
        /// Overwrite existing config without prompting
        #[arg(long)]
        force: bool,
    },
    /// Inspect the tool-call audit log
    Audit {
        #[command(subcommand)]
        sub: AuditCmd,
    },
}

#[derive(Debug, Subcommand)]
pub enum AuditCmd {
    /// Show the last N audit-log entries (default 20)
    Tail {
        #[arg(short = 'n', long, default_value_t = 20)]
        count: usize,
        /// Print raw JSON lines instead of the formatted table
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum ModelsCmd {
    /// List bundled and synced-overlay models
    List,
    /// Refresh the synced models overlay from the Gemini API
    Sync {
        /// Print the diff but don't write the overlay
        #[arg(long)]
        dry_run: bool,
    },
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
