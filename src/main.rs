use clap::{Args, Parser, Subcommand};

use monocle_cli::commands::audio_speech::{audio_speech_command, AudioSpeechOptions};
use monocle_cli::commands::audio_speech_azure::{
    audio_speech_azure_command, AudioSpeechAzureOptions,
};
use monocle_cli::commands::audio_transcribe::{audio_transcribe_command, AudioTranscribeOptions};
use monocle_cli::commands::audio_transcribe_azure::{
    audio_transcribe_azure_command, AudioTranscribeAzureOptions,
};
use monocle_cli::commands::chat::{chat_command, ChatOptions};
use monocle_cli::commands::claude::claude_command;
use monocle_cli::commands::login::login_command;
use monocle_cli::commands::model_list::model_list_command;
use monocle_cli::commands::setup::setup_command;
use monocle_cli::commands::status::status_command;
use monocle_cli::commands::token::token_command;
use monocle_cli::commands::unset::unset_command;
use monocle_cli::credentials::Credentials;
use monocle_cli::error::Result;
use monocle_cli::net::Client;
use monocle_cli::util;

#[derive(Parser)]
#[command(
    name = "monocle",
    version,
    about = "CLI authentication tool for Claude Code with Stark OIDC integration"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Authenticate with Stark OIDC provider
    Login {
        /// Stark tenant domain (e.g., example.monocle-ai.com)
        #[arg(long)]
        tenant: Option<String>,
        /// Environment: prod, stg, local (default: prod)
        #[arg(long, default_value = "prod")]
        env: String,
        /// Use Device Authorization Grant (for headless/SSH environments)
        #[arg(long = "device-code")]
        device_code: bool,
    },
    /// Output access token to stdout (for apiKeyHelper)
    Token,
    /// Configure Claude Code to use Monocle authentication
    Setup,
    /// Remove Monocle configuration from Claude Code
    Unset,
    /// Show authentication and configuration status
    Status,
    /// Launch Claude Code with Monocle authentication (clears conflicting env vars)
    #[command(disable_help_flag = true)]
    Claude {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
        args: Vec<String>,
    },
    /// List available models from the Monocle router
    Models,
    /// Chat with LLM via Monocle router (interactive REPL or pipe from stdin)
    Chat(ChatArgs),
    /// Call audio (STT / TTS) endpoints directly for debugging
    Audio {
        #[command(subcommand)]
        command: AudioCommands,
    },
    /// [Deprecated] Use `monocle chat` / `monocle models` instead
    #[command(hide = true)]
    Model {
        #[command(subcommand)]
        command: ModelCommands,
    },
}

#[derive(Args)]
struct ChatArgs {
    /// Model ID to use
    #[arg(long, default_value = "claude-sonnet-4-6")]
    model: String,
    /// System prompt text
    #[arg(long = "system-prompt")]
    system_prompt: Option<String>,
    /// Load system prompt from file
    #[arg(long = "system-prompt-file")]
    system_prompt_file: Option<String>,
    /// Maximum output tokens
    #[arg(long = "max-tokens", default_value = "4096")]
    max_tokens: String,
}

impl From<ChatArgs> for ChatOptions {
    fn from(a: ChatArgs) -> Self {
        ChatOptions {
            model: Some(a.model),
            system_prompt: a.system_prompt,
            system_prompt_file: a.system_prompt_file,
            max_tokens: Some(a.max_tokens),
        }
    }
}

#[derive(Subcommand)]
enum AudioCommands {
    /// Transcribe audio via /v1/audio/transcriptions (OpenAI compatible)
    Transcribe {
        file: Option<String>,
        /// Model ID (e.g., gpt-4o-mini-transcribe, whisper-1)
        #[arg(long)]
        model: Option<String>,
        /// ISO-639-1 language hint
        #[arg(long)]
        language: Option<String>,
        /// Optional prompt to guide the transcription
        #[arg(long)]
        prompt: Option<String>,
        /// json | text | srt | verbose_json | vtt
        #[arg(long = "response-format")]
        response_format: Option<String>,
        /// Sampling temperature (0-1)
        #[arg(long)]
        temperature: Option<String>,
        /// Filename to send (required when piping stdin without extension)
        #[arg(long)]
        filename: Option<String>,
        /// Override MIME type (e.g., audio/wav)
        #[arg(long = "content-type")]
        content_type: Option<String>,
    },
    /// Transcribe via Azure Fast endpoint /v1/speechtotext/transcriptions:transcribe
    #[command(name = "transcribe-azure")]
    TranscribeAzure {
        file: Option<String>,
        /// Locale (e.g., en-US, ko-KR) — repeatable
        #[arg(long = "locale")]
        locale: Vec<String>,
        /// Enable speaker diarization
        #[arg(long)]
        diarization: bool,
        /// None | Removed | Masked | Tags
        #[arg(long)]
        profanity: Option<String>,
        /// Comma-separated channel indices (e.g., "0,1")
        #[arg(long)]
        channels: Option<String>,
        /// Raw definition JSON (escape hatch; overrides individual flags)
        #[arg(long)]
        definition: Option<String>,
        /// Filename to send (required when piping stdin without extension)
        #[arg(long)]
        filename: Option<String>,
        /// Override MIME type (e.g., audio/wav)
        #[arg(long = "content-type")]
        content_type: Option<String>,
    },
    /// Synthesize speech via /v1/audio/speech (OpenAI compatible)
    Speech {
        text: Option<String>,
        /// Model ID (e.g., gpt-4o-mini-tts)
        #[arg(long)]
        model: Option<String>,
        /// Voice ID (e.g., alloy, echo, fable, onyx, nova, shimmer)
        #[arg(long)]
        voice: Option<String>,
        /// Output format (mp3 | opus | aac | flac | wav | pcm)
        #[arg(long)]
        format: Option<String>,
        /// Speech speed (0.25-4.0)
        #[arg(long)]
        speed: Option<String>,
        /// Style/delivery instructions (model-dependent)
        #[arg(long)]
        instructions: Option<String>,
        /// Write audio to this path instead of stdout
        #[arg(short = 'o', long)]
        output: Option<String>,
    },
    /// Synthesize speech via Azure /v1/azure/texttospeech/cognitiveservices/v1 (SSML body)
    #[command(name = "speech-azure")]
    SpeechAzure {
        ssml: Option<String>,
        /// X-Microsoft-OutputFormat (e.g., audio-24khz-48kbitrate-mono-mp3)
        #[arg(long)]
        format: Option<String>,
        /// Write audio to this path instead of stdout
        #[arg(short = 'o', long)]
        output: Option<String>,
    },
}

#[derive(Subcommand)]
enum ModelCommands {
    /// [Deprecated] Use `monocle models` instead
    List,
    /// [Deprecated] Use `monocle chat` instead
    Chat(ChatArgs),
}

fn main() {
    let cli = Cli::parse();
    let client = Client::new();
    let creds = Credentials::new();
    let home = util::home_dir();

    let result: Result<()> = match cli.command {
        Commands::Login {
            tenant,
            env,
            device_code,
        } => login_command(&client, &creds, tenant, env, device_code),
        Commands::Token => {
            token_command(&client, &creds);
            Ok(())
        }
        Commands::Setup => setup_command(&creds, &home),
        Commands::Unset => unset_command(&home),
        Commands::Status => {
            status_command(&creds, &home);
            Ok(())
        }
        Commands::Claude { args } => {
            claude_command(&creds, &args);
            Ok(())
        }
        Commands::Models => model_list_command(&client, &creds),
        Commands::Chat(args) => chat_command(&client, &creds, args.into()),
        Commands::Audio { command } => run_audio(&client, &creds, command),
        Commands::Model { command } => {
            match command {
                ModelCommands::List => {
                    eprintln!("Warning: `monocle model list` is deprecated. Use `monocle models` instead.");
                    model_list_command(&client, &creds)
                }
                ModelCommands::Chat(args) => {
                    eprintln!(
                        "Warning: `monocle model chat` is deprecated. Use `monocle chat` instead."
                    );
                    chat_command(&client, &creds, args.into())
                }
            }
        }
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

fn run_audio(client: &Client, creds: &Credentials, command: AudioCommands) -> Result<()> {
    match command {
        AudioCommands::Transcribe {
            file,
            model,
            language,
            prompt,
            response_format,
            temperature,
            filename,
            content_type,
        } => audio_transcribe_command(
            client,
            creds,
            file.as_deref(),
            AudioTranscribeOptions {
                model,
                language,
                prompt,
                response_format,
                temperature,
                filename,
                content_type,
            },
        ),
        AudioCommands::TranscribeAzure {
            file,
            locale,
            diarization,
            profanity,
            channels,
            definition,
            filename,
            content_type,
        } => audio_transcribe_azure_command(
            client,
            creds,
            file.as_deref(),
            AudioTranscribeAzureOptions {
                locales: if locale.is_empty() {
                    None
                } else {
                    Some(locale)
                },
                diarization,
                profanity,
                channels,
                definition,
                filename,
                content_type,
            },
        ),
        AudioCommands::Speech {
            text,
            model,
            voice,
            format,
            speed,
            instructions,
            output,
        } => audio_speech_command(
            client,
            creds,
            text.as_deref(),
            AudioSpeechOptions {
                model,
                voice,
                format,
                speed,
                instructions,
                output,
            },
        ),
        AudioCommands::SpeechAzure {
            ssml,
            format,
            output,
        } => audio_speech_azure_command(
            client,
            creds,
            ssml.as_deref(),
            AudioSpeechAzureOptions { format, output },
        ),
    }
}
