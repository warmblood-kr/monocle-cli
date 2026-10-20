use clap::{Args, Parser, Subcommand};

use monocle_cli::commands::agent::{agent_command, AgentOptions};
use monocle_cli::commands::audio_speech::{audio_speech_command, AudioSpeechOptions};
use monocle_cli::commands::audio_speech_azure::{
    audio_speech_azure_command, AudioSpeechAzureOptions,
};
use monocle_cli::commands::audio_transcribe::{audio_transcribe_command, AudioTranscribeOptions};
use monocle_cli::commands::audio_transcribe_azure::{
    audio_transcribe_azure_command, AudioTranscribeAzureOptions,
};
use monocle_cli::commands::chat::{
    chat_command, chat_content_command, chat_delete_command, chat_list_command, ChatOptions,
    ToolFiringExpectation,
};
use monocle_cli::commands::claude::claude_command;
use monocle_cli::commands::craft::{
    craft_agent_command, craft_session_command, craft_turn_command, craft_workspace_command,
    AgentCommand, Lane, SessionCommand, WorkspaceCommand,
};
use monocle_cli::commands::craft_turn::{Approve, TurnOpts};
use monocle_cli::commands::image_edit::{image_edit_command, ImageEditOptions};
use monocle_cli::commands::image_generate::{image_generate_command, ImageGenerateOptions};
use monocle_cli::commands::knowledge::{
    knowledge_ask_command, knowledge_content_command, knowledge_create_command,
    knowledge_delete_command, knowledge_list_command, knowledge_remove_command,
    knowledge_replace_command, knowledge_reprocess_command, knowledge_update_command,
    knowledge_upload_command,
};
use monocle_cli::commands::login::login_command;
use monocle_cli::commands::mcp::{
    mcp_attention_command, mcp_exec_command, mcp_ls_command, mcp_set_command, mcp_show_command,
    mcp_thread_command,
};
use monocle_cli::commands::model_list::model_list_command;
use monocle_cli::commands::schedule::{
    schedule_create_command, schedule_delete_command, schedule_list_command, schedule_mcp_command,
    schedule_preview_command, schedule_runs_command, schedule_show_command,
    schedule_update_command, WriteArgs as ScheduleWriteArgs,
};
use monocle_cli::commands::setup::setup_command;
use monocle_cli::commands::status::status_command;
use monocle_cli::commands::token::token_command;
use monocle_cli::commands::unset::unset_command;
use monocle_cli::commands::upgrade::upgrade_command;
use monocle_cli::commands::whoami::whoami_command;
use monocle_cli::commands::workspace_model::{self as wm, WriteArgs};
use monocle_cli::config::Config;
use monocle_cli::credentials::Credentials;
use monocle_cli::diag;
use monocle_cli::error::Result;
use monocle_cli::jarvice_chats::ContentFormat;
use monocle_cli::net::Client;
use monocle_cli::profiles::{self, Selection};
use monocle_cli::update_notice;
use monocle_cli::util;

// clap 4 has no native way to group subcommands under section headings, so the
// root command uses a custom help_template that drops the flat auto-generated
// command list ({options} still renders flags) and prints a hand-grouped list
// via {after-help}. Only the root sets this template, so subcommand `--help`
// screens keep clap's default layout.
const ROOT_HELP_TEMPLATE: &str = "\
{about-with-newline}
{usage-heading} {usage}{after-help}

Options:
{options}";

const ROOT_HELP_COMMANDS: &str = "\
Commands:
  Auth & profile:
    login            Log in to your Monocle tenant
    token            Print the access token (for apiKeyHelper)
    setup            Configure Claude Code to use Monocle authentication
    unset            Remove Monocle configuration from Claude Code
    status           Show authentication and configuration status
    whoami           Show who this profile is signed in as
    profile          List, inspect, and select saved profiles
    claude           Launch Claude Code with Monocle authentication

  Chat & agents:
    chat             Chat with a model (REPL, or pipe a prompt on stdin)
    craft            Run Craft agent turns and manage Craft sessions
    agent            [Experimental] Headless agent loop with tools
    acp              [Experimental] Run as an ACP agent over stdio

  Workspace data:
    knowledge        Manage knowledge collections and ask questions
    mcp              Inspect and toggle your MCP servers and their tools
    schedule         Inspect and manage your schedules
    workspace-model  Manage workspace (custom) models

  Router:
    router           Call the OpenAI-compatible Monocle router directly

  Maintenance:
    upgrade          Update monocle to the latest release
    help             Print this message or the help of a subcommand

Run `monocle <command> --help` for details and examples.";

#[derive(Parser)]
#[command(
    name = "monocle",
    version,
    about = "Control and use Monocle AI from your terminal: log in once, then chat with models, run the agent, or integrate Claude Code.",
    help_template = ROOT_HELP_TEMPLATE,
    after_help = ROOT_HELP_COMMANDS
)]
struct Cli {
    /// Select a saved Monocle connection
    #[arg(long, global = true)]
    profile: Option<String>,
    /// Select a saved environment slot (prod, stg, local)
    #[arg(long, global = true)]
    env: Option<String>,
    /// Show protocol detail on stderr
    #[arg(short = 'v', long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Log in to your Monocle tenant
    Login {
        /// Tenant domain (e.g., example.monocle-ai.com)
        #[arg(long)]
        tenant: Option<String>,
        /// Use Device Authorization Grant (for headless/SSH environments)
        #[arg(long = "device-code")]
        device_code: bool,
    },
    /// Print the access token (for apiKeyHelper)
    Token,
    /// Configure Claude Code to use Monocle authentication
    Setup,
    /// Remove Monocle configuration from Claude Code
    Unset,
    /// Show authentication and configuration status
    Status,
    /// Show who this profile is signed in as (local, no network by default)
    Whoami {
        /// Also ask the server who it thinks you are. Calls GET /api/v1/auths/, which creates a
        /// server-side session token as a side effect (the CLI discards it). 401/403 means the
        /// tenant does not accept this login token or the token has no email claim (a known
        /// staging issue)
        #[arg(long)]
        server: bool,
        /// Print machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// List, inspect, and select saved profiles
    Profile {
        #[command(subcommand)]
        command: ProfileCommands,
    },
    /// Launch Claude Code with Monocle authentication
    ///
    /// Clears conflicting ANTHROPIC_* env vars. Arguments are passed through to `claude`.
    #[command(disable_help_flag = true, after_help = CLAUDE_HELP_HINT)]
    Claude {
        /// Arguments passed to `claude` unchanged
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
        args: Vec<String>,
    },
    /// [Experimental] Run as an ACP agent over stdio
    ///
    /// For editors, the desktop app and Craft.
    Acp,
    /// [Deprecated] Use `monocle router models` instead
    #[command(hide = true)]
    Models,
    /// Call the OpenAI-compatible Monocle router directly
    ///
    /// Useful for debugging, evals and scripting (chat, models, image, audio).
    Router {
        #[command(subcommand)]
        command: RouterCommands,
    },
    /// Chat with a model (REPL, or pipe a prompt on stdin)
    ///
    /// Chats are stored on the server and show up in the web UI. Start an interactive
    /// REPL on a terminal, or pipe the prompt for a one-shot answer on stdout.
    #[command(after_help = CHAT_EXAMPLES)]
    Chat {
        #[command(subcommand)]
        command: Option<ChatCommand>,
        #[command(flatten)]
        args: ChatArgs,
    },
    /// Manage knowledge collections and ask questions
    ///
    /// Subcommands: create, upload, list, update, remove, replace, reprocess,
    /// content, ask, delete.
    Knowledge {
        #[command(subcommand)]
        command: KnowledgeCommand,
    },
    /// Inspect and toggle your MCP servers and their tools
    ///
    /// Subcommands: ls, show, attention, enable, disable, exec, tool, thread.
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    /// Inspect and manage your schedules
    ///
    /// Subcommands: list, show, toggle, delete, runs, mcp-servers, preview, create,
    /// update.
    Schedule {
        #[command(subcommand)]
        command: ScheduleCommand,
    },
    /// Manage workspace (custom) models
    ///
    /// A workspace model bundles a system prompt, tools and knowledge; use it with
    /// `monocle chat --model <id>`.
    WorkspaceModel {
        #[command(subcommand)]
        command: WorkspaceModelCommand,
    },
    /// Run Craft agent turns and manage Craft sessions
    ///
    /// Subcommands: turn, session, workspace, agent.
    #[command(after_long_help = CRAFT_EXAMPLES)]
    Craft {
        /// Craft backend: the Craft server (openworker) or Claude-SDK agent sessions (sdk)
        #[arg(long, value_enum, default_value = "openworker", global = true)]
        lane: Lane,
        #[command(subcommand)]
        command: CraftCommand,
    },
    /// [Experimental] Headless agent loop with tools
    ///
    /// The agent can read, write and edit files and run shell commands.
    Agent {
        /// The task/prompt (optional; omit to type it interactively, or pipe via stdin)
        prompt: Option<String>,
        /// Working directory the agent acts in (default: current directory)
        #[arg(long)]
        workdir: Option<String>,
        /// Model id to use (routed via Monocle)
        #[arg(long, default_value = monocle_cli::agent::DEFAULT_MODEL)]
        model: String,
        /// Maximum loop steps before giving up
        #[arg(long = "max-steps", default_value_t = monocle_cli::agent::DEFAULT_MAX_STEPS)]
        max_steps: usize,
        /// Named session to persist/resume (~/.monocle/agent/<name>.jsonl)
        #[arg(long)]
        session: Option<String>,
        /// Skip the per-tool approval prompt (dangerous)
        #[arg(long = "auto-approve")]
        auto_approve: bool,
    },
    /// [Deprecated] Use `monocle router audio` instead
    #[command(hide = true)]
    Audio {
        #[command(subcommand)]
        command: AudioCommands,
    },
    /// [Deprecated] Use `monocle router image` instead
    #[command(hide = true)]
    Image {
        #[command(subcommand)]
        command: ImageCommands,
    },
    /// [Deprecated] Use `monocle router chat` instead
    #[command(hide = true)]
    Model {
        #[command(subcommand)]
        command: ModelCommands,
    },
    /// Update monocle to the latest release
    Upgrade {
        /// Only check for a newer version, don't install
        #[arg(long)]
        check: bool,
    },
}

#[derive(Subcommand)]
enum ProfileCommands {
    /// List saved profiles and their environments
    List,
    /// Show one profile and its login state
    Show { name: Option<String> },
    /// Set the default profile
    Use { name: String },
}

#[derive(Args)]
struct ChatArgs {
    /// Model ID to use
    #[arg(long, default_value = monocle_cli::agent::DEFAULT_MODEL)]
    model: String,
    /// Router-only; ignored with a warning (use `monocle router chat`).
    #[arg(long = "system-prompt", hide = true)]
    system_prompt: Option<String>,
    /// Router-only; ignored with a warning (use `monocle router chat`).
    #[arg(long = "system-prompt-file", hide = true)]
    system_prompt_file: Option<String>,
    /// Router-only; ignored with a warning (use `monocle router chat`).
    #[arg(long = "max-tokens", hide = true)]
    max_tokens: Option<String>,
    /// Attach a file/image (repeatable); local path or http(s) URL. One-shot
    /// (piped stdin) only — see also inband `file:<path>` tokens in the text.
    #[arg(long = "file")]
    file: Vec<String>,
    /// Hidden no-op kept until 2.0: plain `monocle chat` is already hosted
    /// Chat (`/api/responses`).
    #[arg(long, hide = true)]
    responses: bool,
    /// Continue this existing chat (ID printed after each reply, or see `monocle chat list`)
    /// instead of starting a new one. When entering the interactive REPL,
    /// its prior history is replayed to stderr before the prompt.
    #[arg(long)]
    resume: Option<String>,
    /// MCP/connected-app server ids to activate for this turn
    /// (comma-separated). Without this, a turn only gets tools from the
    /// model's own static config — there is no other way to select which MCP
    /// servers are on for a given call.
    #[arg(long = "tool-ids", value_delimiter = ',')]
    tool_ids: Vec<String>,
    /// Assert from the server's `tools_used` that a
    /// server-executed tool actually RAN, as a scriptable exit code. Pass a
    /// tool name to require THAT tool (`--verify-tool-firing=web_search`);
    /// bare, it only requires that at least one tool ran. Exit codes: 0 = the
    /// expected tool ran, 1 = it did not (a different tool ran, none ran, or an
    /// unresolved tool_calls report leaked back), 2 = could not determine (this
    /// server does not report `tools_used`, or the entries were unreadable).
    /// One-shot (piped input) only. The answer is written to stdout first
    /// regardless of the verdict.
    #[arg(long = "verify-tool-firing", num_args = 0..=1)]
    verify_tool_firing: Option<Option<String>>,
    /// After each reply, print the tools the server reports it ran
    /// (`tools_used`) to stderr, e.g. `Tools: web_search, model_designer__delegate_model`.
    #[arg(long = "show-tools")]
    show_tools: bool,
    /// Experimental: streams the answer as it arrives (headless path). The chat title may appear a moment later in the web UI.
    #[arg(long)]
    stream: bool,
    /// One-shot only: print one JSON timings line to stderr (stdout unchanged). t0 is the moment
    /// the request is sent; the reply is one blocking response, so ttfe = ttft = total (no
    /// streaming) unless `--stream` is set (then ttfe = first SSE event, ttft = first answer text, total = stream end). Includes a random run_id (= trace_id, sent as `traceparent` to Monocle hosts only; disable with MONOCLE_NO_TRACEPARENT=1) and started_at (UTC); never tokens, prompts or text
    #[arg(long)]
    timings: bool,
    /// One-shot only: append one JSON line per phase to this file (also MONOCLE_DIAGNOSTICS_FILE);
    /// off by default; fixed fields only, never tokens/prompts/text/headers/URLs; 0600; rotates at 1 MiB
    #[arg(long = "diagnostics-file", value_name = "PATH")]
    diagnostics_file: Option<std::path::PathBuf>,
}

impl From<ChatArgs> for ChatOptions {
    fn from(a: ChatArgs) -> Self {
        ChatOptions {
            model: Some(a.model),
            system_prompt: a.system_prompt,
            system_prompt_file: a.system_prompt_file,
            max_tokens: a.max_tokens,
            files: a.file,
            // 1.8: hosted Chat is the default; `--responses` is a no-op.
            responses: true,
            resume: a.resume,
            tool_ids: a.tool_ids,
            verify_tool_firing: a.verify_tool_firing.map(|v| match v {
                Some(name) => ToolFiringExpectation::Named(name),
                None => ToolFiringExpectation::Any,
            }),
            show_tools: a.show_tools,
            stream: a.stream,
            timings: a.timings,
            diagnostics_file: a.diagnostics_file,
        }
    }
}

/// `monocle router chat` flags: plain `/v1/chat/completions` only (no
/// Responses-API options).
#[derive(Args)]
struct RouterChatArgs {
    /// Model ID to use
    #[arg(long, default_value = monocle_cli::agent::DEFAULT_MODEL)]
    model: String,
    /// System prompt text
    #[arg(long = "system-prompt")]
    system_prompt: Option<String>,
    /// Load system prompt from file
    #[arg(long = "system-prompt-file")]
    system_prompt_file: Option<String>,
    /// Maximum output tokens (omitted by default → model/router uses its own)
    #[arg(long = "max-tokens")]
    max_tokens: Option<String>,
    /// Attach a file/image (repeatable); local path or http(s) URL. One-shot
    /// (piped stdin) only — see also inband `file:<path>` tokens in the text.
    #[arg(long = "file")]
    file: Vec<String>,
    /// One-shot only: print one JSON timings line to stderr (stdout unchanged). Streamed, so
    /// ttfe = ttft = first answer chunk after the request is sent; total = stream end
    #[arg(long)]
    timings: bool,
}

impl From<RouterChatArgs> for ChatOptions {
    fn from(a: RouterChatArgs) -> Self {
        ChatOptions {
            model: Some(a.model),
            system_prompt: a.system_prompt,
            system_prompt_file: a.system_prompt_file,
            max_tokens: a.max_tokens,
            files: a.file,
            responses: false,
            resume: None,
            tool_ids: Vec::new(),
            verify_tool_firing: None,
            show_tools: false,
            stream: false,
            timings: a.timings,
            diagnostics_file: None,
        }
    }
}

fn profile_list_command(config: &Config, home: &std::path::Path) -> Result<()> {
    let data = profiles::list_profiles(config)?;
    let selected = data.default_profile.as_deref().unwrap_or("default");
    let mut names = data
        .profiles
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    names.insert("default".to_string());
    if let Some(default) = data.default_profile.as_ref() {
        names.insert(default.clone());
    }
    for name in names {
        let selection = profiles::resolve_from_data(&data, home, Some(&name), None, None)?;
        let store = selection.store(home);
        let state = if store.read().is_some() {
            "logged in"
        } else {
            "not logged in"
        };
        let marker = if name == selected { "*" } else { " " };
        println!("{marker} {name}  {}  ({state})", selection.environment);
    }
    Ok(())
}

fn profile_show_command(
    config: &Config,
    home: &std::path::Path,
    name: Option<&str>,
    current: &Selection,
) -> Result<()> {
    let data = profiles::list_profiles(config)?;
    let profile = name.unwrap_or(&current.profile);
    if profile != "default" && !data.profiles.contains_key(profile) {
        return Err(monocle_cli::error::AppError::new(
            profiles::unknown_profile_message(profile),
        ));
    }
    let selection = profiles::resolve_from_data(&data, home, Some(profile), None, None)?;
    let store = selection.store(home);
    println!("Profile: {}", selection.profile);
    println!("Environment: {}", selection.environment);
    match store.read() {
        Some(creds) => {
            println!("Tenant: {}", creds.tenant_domain);
            println!("User: {}", creds.email);
            println!("Status: logged in");
        }
        None => println!("Status: not logged in"),
    }
    Ok(())
}

#[derive(Subcommand)]
enum ChatCommand {
    /// List your chats (id, title, last updated)
    ///
    /// Includes chats created in the web UI, since both share the same storage.
    List,
    /// Print one chat's active message path to stdout (text only; untrusted content)
    Content {
        /// Chat ID (see `monocle chat list`)
        #[arg(value_name = "CHAT_ID")]
        id: String,
        #[arg(long, value_enum, default_value = "text")]
        format: ContentFormat,
        /// Replace each tool-call block with a one-line summary (tool name,
        /// sub-agent `model_id`, `report_to_parent`); with `--format json`,
        /// add a structured `tool_calls` list per message
        #[arg(long = "show-tools")]
        show_tools: bool,
    },
    /// Permanently delete one chat (not undoable, never retried)
    ///
    /// The server answers with an error (HTTP 401 or 500) for a chat id that does not
    /// exist or is not yours; that is reported as "not found or not yours".
    Delete {
        /// Chat ID (see `monocle chat list`)
        #[arg(value_name = "CHAT_ID")]
        id: String,
    },
}

const CHAT_EXAMPLES: &str = "\
Examples:
  monocle chat                                 # interactive REPL
  echo \"summarize the plan\" | monocle chat    # one-shot: pipe the prompt, answer on stdout
  monocle chat --resume CHAT_ID                # continue an earlier chat
  monocle chat list                            # find chat IDs";

const KNOWLEDGE_ASK_EXAMPLES: &str = "\
Examples:
  monocle knowledge ask handbook \"What is the vacation policy?\"
  monocle knowledge ask handbook \"Who owns onboarding?\" --json";

const SCHEDULE_CREATE_EXAMPLES: &str = "\
Examples:
  monocle schedule create --model claude-sonnet-5 --prompt \"daily digest\" --at +1h
  monocle schedule create --model claude-sonnet-5 --prompt \"daily digest\" \\
    --rrule FREQ=DAILY --dtstart 2030-01-02T09:00:00+09:00 --timezone Asia/Seoul
  monocle schedule create --dry-run ...         # only print the next fire times";

const MCP_EXEC_EXAMPLES: &str = "\
Examples:
  monocle mcp exec github                       # the model calls any tool of the server
  monocle mcp exec github/list_issues --verify  # exit 0 only if that tool ran
  monocle mcp exec github \"list my open issues\"  # your own prompt";

const CLAUDE_WRAPPER_HELP: &str = "\
Launch Claude Code with Monocle authentication

Usage: monocle [--profile P] [--env E] claude [CLAUDE_ARGS]...

Monocle points Claude Code at your tenant's router, installs a token helper and
clears conflicting ANTHROPIC_* env vars for this process only. Everything after
`claude` is passed to the `claude` binary unchanged. For Claude Code's own
options run `claude --help` directly.

Examples:
  monocle claude                          # start Claude Code
  monocle claude -p \"explain this repo\"   # Claude Code flags pass through";

const CLAUDE_HELP_HINT: &str = "\
Everything after `claude` is passed to Claude Code unchanged.
Run `monocle claude --help` for details.";

const CRAFT_EXAMPLES: &str = "\
Examples:
  monocle craft turn \"summarize this repo\"           # default lane: Craft server
  monocle craft turn --lane sdk \"hello\"              # Claude-SDK lane (agent sandbox)
  monocle craft turn --lane sdk --session ID \"more\"  # continue a session
  monocle craft session list                          # sessions on the chosen lane";

const TURN_EXAMPLES: &str = "\
Examples:
  Choose the lane with --lane:
    monocle craft turn \"hello\"                  # openworker (default): Craft server
    monocle craft turn --lane sdk \"hello\"       # sdk: Claude-SDK lane via the agent sandbox

  Continue a conversation with --session ID (the ID is printed on stderr after each turn):
    monocle craft turn --session ID \"and then?\"
    monocle craft turn --lane sdk --session ID \"and then?\"

  Talk interactively (one line per message, one session; /exit or Ctrl-D ends):
    monocle craft turn -i
    monocle craft turn -i \"hello\"             # TEXT is the first message
    monocle craft turn -i --continue            # resume the last session and keep talking
    printf 'hi\\nand then?\\n' | monocle craft turn -i --json";

#[derive(Subcommand)]
enum CraftCommand {
    /// Manage Craft sessions
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Recent workspaces (openworker lane only)
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
    /// Agents offered by the Craft server (openworker lane only)
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    /// Send one message to a Craft agent and print its answer
    ///
    /// Works on both lanes; --workspace, --agent, --approve and --no-delegation apply
    /// to the openworker lane only.
    #[command(after_long_help = TURN_EXAMPLES)]
    Turn {
        /// The message to send (optional with -i)
        #[arg(required_unless_present = "interactive")]
        text: Option<String>,
        /// Interactive: read one message per line from stdin on ONE session (the first creates it,
        /// or --session/--continue resumes it; TEXT, if given, is the first message). `/exit`, `/quit`
        /// or EOF ends it. Blank lines are skipped; `> ` is shown only on a terminal. Options apply
        /// per turn (--timeout, --timings, --diagnostics-file). A failed first turn exits non-zero;
        /// a later failure is printed to stderr and the loop continues. This is a flag on `turn`
        /// (not a `craft chat` command) so all turn options stay in one place. The connection is
        /// re-established for every turn. A queue_full rejection is retried up to 3 times (3 s apart)
        #[arg(short = 'i', long)]
        interactive: bool,
        /// Continue this session (default: a new session; its ID is printed to stderr)
        #[arg(long)]
        session: Option<String>,
        /// Continue the most recent session of this lane + profile + env (conflicts with --session)
        #[arg(long = "continue", conflicts_with = "session")]
        continue_last: bool,
        /// Workspace folder to run in (opened first). Default: your most recent existing one
        #[arg(long)]
        workspace: Option<String>,
        /// Create the --workspace folder if it does not exist
        #[arg(long, requires = "workspace")]
        create_workspace: bool,
        /// Agent to talk to (see `craft agent list`)
        #[arg(long, default_value = "cowork")]
        agent: String,
        /// Give up after this many seconds
        #[arg(long, default_value_t = 180)]
        timeout: u64,
        /// Do not attach your delegation to the session first
        #[arg(long)]
        no_delegation: bool,
        /// Title for the new session (sdk lane only; default "CLI turn")
        #[arg(long)]
        title: Option<String>,
        /// Answer for permission prompts (a CLI turn cannot ask interactively)
        #[arg(long, value_enum, default_value = "deny")]
        approve: Approve,
        /// Print each received frame / polled message as one JSON line instead of text
        #[arg(long)]
        json: bool,
        /// After the turn, print one JSON timings line to stderr (stdout unchanged). t0 is the
        /// moment the user message is handed to the socket; ttfe = first server frame after t0,
        /// ttft = first non-empty assistant text, total = turn end (all ms from t0, monotonic;
        /// null if it never happened). t_request_ms = ms from command start to t0; connect_ms,
        /// delegation_ms, session_create_ms are setup phases. Includes a random run_id (= trace_id, sent as
        /// `traceparent` to Monocle hosts only; disable with MONOCLE_NO_TRACEPARENT=1) and started_at (UTC); never tokens, prompts or text. If the answer is polled, ttft is accurate to the 2 s poll interval
        #[arg(long)]
        timings: bool,
        /// Append one JSON line per phase (connect, delegation, session_create, first_event,
        /// first_text, done/error) to this file; also set by MONOCLE_DIAGNOSTICS_FILE. Off by
        /// default. Fixed fields only (ts, run_id, trace_id, event, lane, host, error_kind, duration_ms): never
        /// tokens, prompts, text, headers or URLs. Created 0600; rotated to PATH.1 at 1 MiB
        #[arg(long = "diagnostics-file", value_name = "PATH")]
        diagnostics_file: Option<std::path::PathBuf>,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Toggle {
    Enable,
    Disable,
}

#[derive(Subcommand)]
enum McpCommand {
    /// List MCP servers (name, enabled, auth state, tool count)
    Ls {
        /// Print the server's JSON body unchanged
        #[arg(long)]
        json: bool,
    },
    /// Show one server and its tools with their enabled state
    Show {
        /// Server name or id (see `monocle mcp ls`)
        server: String,
        /// Print the server's JSON body unchanged
        #[arg(long)]
        json: bool,
    },
    /// Count servers whose authentication failed and needs attention
    Attention {
        /// Print the server's JSON body unchanged
        #[arg(long)]
        json: bool,
    },
    /// Enable a server for your account
    Enable {
        /// Server name (see `monocle mcp ls`)
        server: String,
    },
    /// Disable a server for your account
    Disable {
        /// Server name (see `monocle mcp ls`)
        server: String,
    },
    /// Run an MCP server's tool through one chat turn
    ///
    /// LLM-driven: there is no direct tool-invoke API. Prints the reply to stdout, the
    /// chat id to stderr. With --verify, exit code: 0 a tool of it ran, 1 none did
    /// (or a dropped tool call), 2 server cannot report `tools_used`.
    /// Pre-flight failures (unknown/disabled/unauthenticated) also exit 1.
    #[command(after_help = MCP_EXEC_EXAMPLES)]
    Exec {
        /// `SERVER` (any one tool) or `SERVER/TOOL`
        target: String,
        /// Prompt (replaces the built-in "call the tool now" instruction)
        prompt: Vec<String>,
        /// Assert from the server's `tools_used` that the tool actually ran
        #[arg(long)]
        verify: bool,
        /// Model id (default: the CLI default)
        #[arg(long)]
        model: Option<String>,
    },
    /// Switch one tool of an enabled server on or off (does not run the tool)
    Tool {
        /// Server name
        server: String,
        /// Tool name (see `monocle mcp show`)
        tool: String,
        /// New state
        #[arg(value_enum)]
        state: Toggle,
    },
    /// Per-chat MCP server selection (a display mirror, not proof a tool fired)
    Thread {
        #[command(subcommand)]
        command: McpThreadCommand,
    },
}

#[derive(Subcommand)]
enum McpThreadCommand {
    /// Print the MCP server ids set on a chat
    ///
    /// This is a display mirror (the ChatMcpEnabled table): it does NOT prove a tool
    /// fires; to confirm, run `chat --tool-ids mcp:SERVER --verify-tool-firing`
    Get {
        /// Chat id (see `monocle chat list`)
        chat_id: String,
        /// Print the server's JSON body unchanged
        #[arg(long)]
        json: bool,
    },
    /// REPLACE the whole MCP server set of a chat
    ///
    /// This is a display mirror (the ChatMcpEnabled table): it does NOT guarantee the
    /// tool is called; to confirm, run `chat --tool-ids mcp:SERVER --verify-tool-firing`.
    /// Ids are catalog ids from `monocle mcp ls`; the server does not reject unknown ids
    Set {
        /// Chat id (see `monocle chat list`)
        chat_id: String,
        /// Server ids to set
        servers: Vec<String>,
        /// Comma-separated server ids to set (same as listing them)
        #[arg(long = "servers", value_delimiter = ',')]
        servers_flag: Vec<String>,
        /// Explicitly clear the set (required when no servers are given)
        #[arg(long, conflicts_with_all = ["servers", "servers_flag"])]
        none: bool,
        /// Print the server's JSON body unchanged
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ScheduleCommand {
    /// List your schedules
    List {
        /// Print the server's JSON body unchanged (epochs in seconds)
        #[arg(long)]
        json: bool,
    },
    /// Show one schedule
    Show {
        /// Schedule id (see `monocle schedule list`)
        id: String,
        /// Print the server's JSON body unchanged
        #[arg(long)]
        json: bool,
    },
    /// Switch a schedule on (`enable`) or off (`disable`)
    Toggle {
        /// Schedule ID (see `monocle schedule list`)
        id: String,
        #[arg(value_enum)]
        state: Toggle,
        /// Print the server's JSON body unchanged
        #[arg(long)]
        json: bool,
    },
    /// Permanently delete a schedule (single attempt, never retried)
    Delete {
        /// Schedule ID (see `monocle schedule list`)
        id: String,
    },
    /// List a schedule's runs, newest first
    ///
    /// With --verify, exit code (judged on the latest finished run, or --run): 0 a tool
    /// ran, 1 none did, 2 cannot tell (run unfinished, no tool summary, or unrecognized
    /// shape). `--verify=TOOL` expects that tool (`server__tool` sanitized as in
    /// `mcp exec`, or the bare tool name)
    Runs {
        /// Schedule ID (see `monocle schedule list`)
        id: String,
        /// Number of runs to fetch
        #[arg(long, default_value_t = 50)]
        limit: u32,
        /// Print the server's JSON body unchanged
        #[arg(long)]
        json: bool,
        /// Assert from the run's tool summary that a tool fired (optionally TOOL)
        #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "", value_name = "TOOL")]
        verify: Option<String>,
        /// Judge this run id instead of the latest finished run
        #[arg(long, requires = "verify")]
        run: Option<String>,
    },
    /// Show or set a schedule's MCP servers
    ///
    /// A display/selection set for the schedule (it does NOT prove a tool fires; check
    /// with `schedule runs --verify`). Without --set/--none it prints the set; --set
    /// REPLACES the whole set with catalog ids from `monocle mcp ls` (the server does
    /// not reject unknown ids)
    McpServers {
        /// Schedule ID (see `monocle schedule list`)
        id: String,
        /// Server ids to set (space or comma separated)
        #[arg(long, num_args = 0.., value_delimiter = ',')]
        set: Option<Vec<String>>,
        /// Explicitly clear the set (required to empty it)
        #[arg(long, conflicts_with = "set")]
        none: bool,
        /// Print the server's JSON body unchanged
        #[arg(long)]
        json: bool,
    },
    /// Show the next fire times (UTC) of an unsaved schedule; nothing is stored
    #[command(group(clap::ArgGroup::new("when").required(true).args(["recurrence", "run_at"])))]
    Preview {
        /// RFC 5545 RRULE, e.g. FREQ=DAILY (needs --dtstart)
        #[arg(long, requires = "dtstart")]
        recurrence: Option<String>,
        /// First occurrence, epoch seconds
        #[arg(long)]
        dtstart: Option<i64>,
        /// One-off run time, epoch seconds
        #[arg(long, conflicts_with_all = ["recurrence", "dtstart"])]
        run_at: Option<i64>,
        /// IANA timezone
        #[arg(long, default_value = "UTC")]
        timezone: String,
        /// How many fire times
        #[arg(long, default_value_t = 5)]
        count: u32,
        /// Print the server's JSON body unchanged
        #[arg(long)]
        json: bool,
    },
    /// Create a schedule and print its id
    ///
    /// Needs --model, a prompt, and --at or --rrule + --dtstart.
    #[command(after_help = SCHEDULE_CREATE_EXAMPLES)]
    Create {
        #[command(flatten)]
        write: ScheduleWriteArgs,
        /// MCP server id to attach (repeatable; set right after creation, see `monocle mcp ls`)
        #[arg(long = "mcp-server", value_name = "ID")]
        mcp_servers: Vec<String>,
        /// Only print the next fire times; nothing is created
        #[arg(long)]
        dry_run: bool,
        /// Print the server's JSON body unchanged
        #[arg(long)]
        json: bool,
    },
    /// Change a schedule (only the flags you pass change)
    Update {
        /// Schedule ID (see `monocle schedule list`)
        id: String,
        #[command(flatten)]
        write: ScheduleWriteArgs,
        /// Print the server's JSON body unchanged
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum KnowledgeCommand {
    /// Create an empty collection; prints its id to stdout
    Create {
        /// Collection name (must not already exist)
        name: String,
    },
    /// Upload and index files (pdf, docx, md, txt) into a collection
    ///
    /// A directory contributes its supported top-level files; files already in the
    /// collection (same name) are skipped; a failed file is reported and the rest continue
    Upload {
        /// Collection name or id (see `monocle knowledge list`)
        collection: String,
        /// Files and/or directories to upload
        #[arg(required = true, value_name = "FILE_OR_DIR")]
        paths: Vec<String>,
    },
    /// List collections, or the files of one collection
    List {
        /// Collection name or id; omit to list all collections
        collection: Option<String>,
    },
    /// Rename and/or re-describe a collection (sharing/access is left unchanged)
    Update {
        /// Collection name or id
        collection: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        description: Option<String>,
    },
    /// Remove one file from a collection (the collection is kept)
    ///
    /// The server also deletes the file itself unless another collection or chat still
    /// uses it
    Remove {
        /// Collection name or id
        collection: String,
        /// File name or id within the collection (see `monocle knowledge list COLLECTION`)
        file: String,
    },
    /// Replace the same-named file in a collection with a new local version
    Replace {
        /// Collection name or id
        collection: String,
        /// Local file (pdf, docx, md, txt); its name selects the file to replace
        path: String,
    },
    /// Re-extract a file's text on the server and re-index it in the collection
    Reprocess {
        /// Collection name or id
        collection: String,
        /// File name or id within the collection
        file: String,
        /// Re-extract even if the file already has text
        #[arg(long)]
        force: bool,
    },
    /// Print the text the server extracted from a file (check parsing before search)
    Content {
        /// Collection name or id
        collection: String,
        /// File name or id within the collection
        file: String,
    },
    /// Ask a question about a collection
    ///
    /// Prints the answer (stdout) and the files its retrieved passages came from
    /// (stderr). Costs one model turn; not saved to your chats.
    #[command(after_help = KNOWLEDGE_ASK_EXAMPLES)]
    Ask {
        /// Collection name or id
        collection: String,
        /// The question
        question: String,
        /// Model ID to use
        #[arg(long, default_value = monocle_cli::agent::DEFAULT_MODEL)]
        model: String,
        /// Print {"answer", "sources"} as JSON on stdout
        #[arg(long)]
        json: bool,
    },
    /// Delete a collection and all of its files
    Delete {
        /// Collection name or id
        collection: String,
    },
}

#[derive(Args)]
struct ModelWriteArgs {
    /// Model id (what `monocle chat --model` takes)
    id: String,
    /// JSON file with ModelForm fields (base_model_id, name, meta, params, ...); flags override it.
    /// On update, `meta` and `params` merge key-wise onto the stored model (keys not in the file are kept) and other fields are replaced; on create the file is the starting point
    #[arg(long, value_name = "PATH")]
    file: Option<String>,
    /// Display name (default for create: the id)
    #[arg(long)]
    name: Option<String>,
    /// Base model id (see `monocle router models`); required for create unless in --file
    #[arg(long)]
    base: Option<String>,
    #[arg(long)]
    description: Option<String>,
    /// System prompt
    #[arg(long)]
    system: Option<String>,
    /// Tool id, e.g. `mcp:server` (repeatable; replaces the model's tool list)
    #[arg(long = "tool", value_name = "TOOL_ID")]
    tools: Vec<String>,
    /// Knowledge collection name or id (repeatable; replaces the model's collections)
    #[arg(long = "knowledge", value_name = "COLLECTION")]
    knowledge: Vec<String>,
    /// Detach all tools (conflicts with --tool)
    #[arg(long, conflicts_with = "tools")]
    clear_tools: bool,
    /// Detach all knowledge collections (conflicts with --knowledge)
    #[arg(long, conflicts_with = "knowledge")]
    clear_knowledge: bool,
    /// Who can use the model: `private` (owner only; create default) or `public`
    /// (everyone in the tenant). Omit to keep the stored setting on update.
    #[arg(
        long,
        value_parser = ["private", "public"],
        conflicts_with_all = ["read_groups", "write_groups", "read_users", "write_users"]
    )]
    access: Option<String>,
    /// Share: group id allowed to read (repeatable). Any --read-*/--write-* flag
    /// makes the model shared and replaces the whole access list.
    #[arg(long = "read-group", value_name = "GROUP_ID")]
    read_groups: Vec<String>,
    /// Share: group id allowed to write (repeatable)
    #[arg(long = "write-group", value_name = "GROUP_ID")]
    write_groups: Vec<String>,
    /// Share: user id allowed to read (repeatable)
    #[arg(long = "read-user", value_name = "USER_ID")]
    read_users: Vec<String>,
    /// Share: user id allowed to write (repeatable)
    #[arg(long = "write-user", value_name = "USER_ID")]
    write_users: Vec<String>,
}

impl From<ModelWriteArgs> for WriteArgs {
    fn from(a: ModelWriteArgs) -> Self {
        WriteArgs {
            id: a.id,
            file: a.file,
            name: a.name,
            base: a.base,
            description: a.description,
            system: a.system,
            tools: a.tools,
            knowledge: a.knowledge,
            clear_tools: a.clear_tools,
            clear_knowledge: a.clear_knowledge,
            access: a.access,
            read_groups: a.read_groups,
            write_groups: a.write_groups,
            read_users: a.read_users,
            write_users: a.write_users,
        }
    }
}

#[derive(Subcommand)]
enum WorkspaceModelCommand {
    /// List workspace models (id, name, base, active)
    List,
    /// Print one model as JSON
    Show {
        /// Workspace model ID (see `monocle workspace-model list`)
        id: String,
    },
    /// Create a private, active model; prints its id to stdout
    Create(ModelWriteArgs),
    /// Change a model: only the given flags/--file fields change
    Update(ModelWriteArgs),
    /// Flip a model between active and inactive
    Toggle {
        /// Workspace model ID (see `monocle workspace-model list`)
        id: String,
    },
    /// Delete a model
    Delete {
        /// Workspace model ID (see `monocle workspace-model list`)
        id: String,
    },
}

#[derive(Args)]
struct TranscribeArgs {
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
}

#[derive(Args)]
struct TranscribeAzureArgs {
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
}

#[derive(Args)]
struct SpeakArgs {
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
}

#[derive(Args)]
struct SpeakAzureArgs {
    ssml: Option<String>,
    /// X-Microsoft-OutputFormat (e.g., audio-24khz-48kbitrate-mono-mp3)
    #[arg(long)]
    format: Option<String>,
    /// Write audio to this path instead of stdout
    #[arg(short = 'o', long)]
    output: Option<String>,
}

#[derive(Subcommand)]
enum RouterAudioCommands {
    /// Synthesize speech via /v1/audio/speech (OpenAI compatible)
    Speak(SpeakArgs),
    /// Synthesize speech via the Azure endpoint (SSML body)
    ///
    /// Uses /v1/azure/texttospeech/cognitiveservices/v1.
    #[command(name = "speak-azure")]
    SpeakAzure(SpeakAzureArgs),
    /// Transcribe audio via /v1/audio/transcriptions (OpenAI compatible)
    Transcribe(TranscribeArgs),
    /// Transcribe audio via the Azure Fast endpoint
    ///
    /// Uses /v1/speechtotext/transcriptions:transcribe.
    #[command(name = "transcribe-azure")]
    TranscribeAzure(TranscribeAzureArgs),
}

/// Legacy `monocle audio …` names (deprecated; see `legacy_alias`).
#[derive(Subcommand)]
enum AudioCommands {
    /// Transcribe audio via /v1/audio/transcriptions (OpenAI compatible)
    Transcribe(TranscribeArgs),
    /// Transcribe audio via the Azure Fast endpoint
    ///
    /// Uses /v1/speechtotext/transcriptions:transcribe.
    #[command(name = "transcribe-azure")]
    TranscribeAzure(TranscribeAzureArgs),
    /// Synthesize speech via /v1/audio/speech (OpenAI compatible)
    Speech(SpeakArgs),
    /// Synthesize speech via the Azure endpoint (SSML body)
    ///
    /// Uses /v1/azure/texttospeech/cognitiveservices/v1.
    #[command(name = "speech-azure")]
    SpeechAzure(SpeakAzureArgs),
}

#[derive(Subcommand)]
enum ImageCommands {
    /// Generate an image via /v1/images/generations (OpenAI compatible)
    Generate {
        prompt: Option<String>,
        /// Model ID (e.g., gpt-image-1)
        #[arg(long)]
        model: String,
        /// e.g. 1024x1024
        #[arg(long)]
        size: Option<String>,
        /// Number of images to generate
        #[arg(long)]
        n: Option<u32>,
        /// Model-dependent quality setting
        #[arg(long)]
        quality: Option<String>,
        /// Write image(s) here (a second+ image gets `-1`, `-2`, ... suffixed)
        #[arg(short = 'o', long)]
        output: String,
    },
    /// Edit an image via /v1/images/edits (OpenAI compatible; gpt-image models only)
    Edit {
        /// Local image path (png/jpg/webp)
        image: String,
        prompt: Option<String>,
        /// Model ID (must be a gpt-image model)
        #[arg(long)]
        model: String,
        /// Local mask image path (same format as `image`)
        #[arg(long)]
        mask: Option<String>,
        /// e.g. 1024x1024
        #[arg(long)]
        size: Option<String>,
        /// Number of images to generate
        #[arg(long)]
        n: Option<u32>,
        /// Model-dependent quality setting
        #[arg(long)]
        quality: Option<String>,
        /// Write image(s) here (a second+ image gets `-1`, `-2`, ... suffixed)
        #[arg(short = 'o', long)]
        output: String,
    },
}

#[derive(Subcommand)]
enum RouterCommands {
    /// Chat via /v1/chat/completions (interactive REPL or pipe from stdin)
    Chat(RouterChatArgs),
    /// List available models from the Monocle router (with modality)
    Models,
    /// Generate or edit images via /v1/images/* (OpenAI compatible)
    Image {
        #[command(subcommand)]
        command: ImageCommands,
    },
    /// Text-to-speech (speak) and speech-to-text (transcribe)
    ///
    /// OpenAI-compatible and Azure variants.
    Audio {
        #[command(subcommand)]
        command: RouterAudioCommands,
    },
}

#[derive(Subcommand)]
enum ModelCommands {
    /// [Deprecated] Use `monocle router chat` instead
    Chat(RouterChatArgs),
}

fn deprecation(old: &str, new: &str) -> String {
    format!("Warning: `monocle {old}` is deprecated. Use `monocle {new}` instead.")
}

/// Map a deprecated root form (`models`, `audio …`, `image …`, `model chat`) to
/// its warning line and `router` command; anything else comes back unchanged.
// Called once per process on the parsed command; the large `Err` (the
// unchanged `Commands`) is moved, not copied in a hot path.
#[allow(clippy::result_large_err)]
fn legacy_alias(cmd: Commands) -> std::result::Result<(String, RouterCommands), Commands> {
    use RouterAudioCommands as R;
    let (old, new, rc) = match cmd {
        Commands::Models => ("models", "router models", RouterCommands::Models),
        Commands::Model {
            command: ModelCommands::Chat(args),
        } => ("model chat", "router chat", RouterCommands::Chat(args)),
        Commands::Image { command } => {
            let (old, new) = match command {
                ImageCommands::Generate { .. } => ("image generate", "router image generate"),
                ImageCommands::Edit { .. } => ("image edit", "router image edit"),
            };
            (old, new, RouterCommands::Image { command })
        }
        Commands::Audio { command } => {
            let (old, new, command) = match command {
                AudioCommands::Transcribe(a) => (
                    "audio transcribe",
                    "router audio transcribe",
                    R::Transcribe(a),
                ),
                AudioCommands::TranscribeAzure(a) => (
                    "audio transcribe-azure",
                    "router audio transcribe-azure",
                    R::TranscribeAzure(a),
                ),
                AudioCommands::Speech(a) => ("audio speech", "router audio speak", R::Speak(a)),
                AudioCommands::SpeechAzure(a) => (
                    "audio speech-azure",
                    "router audio speak-azure",
                    R::SpeakAzure(a),
                ),
            };
            (old, new, RouterCommands::Audio { command })
        }
        other => return Err(other),
    };
    Ok((deprecation(old, new), rc))
}

/// If the first non-global token is `claude`, return its index: everything after it
/// belongs to Claude Code verbatim (including `-v/--verbose`).
fn claude_split(argv: &[String]) -> Option<usize> {
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "-v" | "--verbose" => {}
            "--profile" | "--env" => i += 1,
            a if a.starts_with("--profile=") || a.starts_with("--env=") => {}
            "claude" => return Some(i),
            _ => return None,
        }
        i += 1;
    }
    None
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let split = claude_split(&argv);
    let head = &argv[..split.map_or(argv.len(), |i| i + 1)];
    let cli = Cli::try_parse_from(head).unwrap_or_else(|e| {
        use clap::error::ErrorKind;
        let _ = e.print();
        // `chat "text"`: the prompt is read from stdin, not taken as an argument.
        let wants_pipe = matches!(
            e.kind(),
            ErrorKind::UnknownArgument | ErrorKind::InvalidSubcommand
        ) && std::env::args().any(|a| a == "chat");
        if wants_pipe {
            eprintln!("\n{}", monocle_cli::commands::chat::PIPE_HINT);
        }
        std::process::exit(e.exit_code());
    });
    monocle_cli::util::set_verbose(cli.verbose);
    let client = Client::new();
    let home = util::home_dir();
    let config = Config::new();
    if let Err(e) = profiles::migrate_legacy(&config, &home) {
        eprintln!("Warning: Failed to migrate config to profiles: {e}");
    }
    let selection =
        match profiles::resolve(&config, &home, cli.profile.as_deref(), cli.env.as_deref()) {
            Ok(selection) => selection,
            Err(e) => {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        };
    if !matches!(&cli.command, Commands::Login { .. }) && selection.profile != "default" {
        match config.read_strict() {
            Ok(data) if data.profiles.contains_key(&selection.profile) => {}
            Ok(_) => {
                eprintln!(
                    "Error: {}",
                    profiles::unknown_profile_message(&selection.profile)
                );
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        }
    }
    let creds = selection.store(&home);

    let command = match legacy_alias(cli.command) {
        Ok((warning, rc)) => {
            eprintln!("{warning}");
            Commands::Router { command: rc }
        }
        Err(c) => c,
    };

    let notice_name = match &command {
        Commands::Token => "token",
        Commands::Upgrade { .. } => "upgrade",
        Commands::Acp => "acp",
        _ => "other",
    };
    let notice = update_notice::start(notice_name, &home);

    let result: Result<()> = match command {
        Commands::Login {
            tenant,
            device_code,
        } => login_command(
            &client,
            &creds,
            tenant,
            selection.environment.clone(),
            device_code,
        )
        .and_then(|_| {
            profiles::save_environment(&config, &selection.profile, &selection.environment)
        }),
        Commands::Token => {
            token_command(&client, &creds);
            Ok(())
        }
        Commands::Setup => setup_command(&creds, &home),
        Commands::Unset => unset_command(&home),
        Commands::Status => {
            status_command(&creds, &home, &selection);
            Ok(())
        }
        Commands::Whoami { server, json } => {
            whoami_command(&client, &creds, &selection, server, json)
        }
        Commands::Profile { command } => match command {
            ProfileCommands::List => profile_list_command(&config, &home),
            ProfileCommands::Show { name } => {
                profile_show_command(&config, &home, name.as_deref(), &selection)
            }
            ProfileCommands::Use { name } => profiles::use_profile(&config, &name),
        },
        Commands::Claude { .. } => {
            let args: Vec<String> = split.map_or(Vec::new(), |i| argv[i + 1..].to_vec());
            if matches!(args.first().map(String::as_str), Some("--help" | "-h")) {
                println!("{CLAUDE_WRAPPER_HELP}");
                std::process::exit(0);
            }
            claude_command(&creds, &args);
            Ok(())
        }
        Commands::Acp => monocle_cli::acp::serve_with_credentials(creds.clone()),
        Commands::Chat { command, args } => match command {
            Some(ChatCommand::List) => chat_list_command(&client, &creds, &selection),
            Some(ChatCommand::Content {
                id,
                format,
                show_tools,
            }) => chat_content_command(&client, &creds, &selection, &id, format, show_tools),
            Some(ChatCommand::Delete { id }) => {
                chat_delete_command(&client, &creds, &selection, &id)
            }
            None => chat_command(&client, &creds, args.into()),
        },
        Commands::Mcp { command } => match command {
            McpCommand::Ls { json } => mcp_ls_command(&client, &creds, &selection, json),
            McpCommand::Show { server, json } => {
                mcp_show_command(&client, &creds, &selection, &server, json)
            }
            McpCommand::Attention { json } => {
                mcp_attention_command(&client, &creds, &selection, json)
            }
            McpCommand::Enable { server } => {
                mcp_set_command(&client, &creds, &selection, &server, None, true)
            }
            McpCommand::Disable { server } => {
                mcp_set_command(&client, &creds, &selection, &server, None, false)
            }
            McpCommand::Exec {
                target,
                prompt,
                verify,
                model,
            } => mcp_exec_command(&client, &creds, &target, &prompt, verify, model.as_deref()),
            McpCommand::Tool {
                server,
                tool,
                state,
            } => mcp_set_command(
                &client,
                &creds,
                &selection,
                &server,
                Some(&tool),
                matches!(state, Toggle::Enable),
            ),
            McpCommand::Thread { command } => match command {
                McpThreadCommand::Get { chat_id, json } => {
                    mcp_thread_command(&client, &creds, &selection, &chat_id, None, json)
                }
                McpThreadCommand::Set {
                    chat_id,
                    mut servers,
                    servers_flag,
                    none,
                    json,
                } => {
                    servers.extend(servers_flag);
                    if servers.is_empty() && !none {
                        Err(monocle_cli::error::AppError::new(
                            "No servers given: list server ids, or pass --none to clear the chat's MCP servers.",
                        ))
                    } else {
                        mcp_thread_command(
                            &client,
                            &creds,
                            &selection,
                            &chat_id,
                            Some(&servers),
                            json,
                        )
                    }
                }
            },
        },
        Commands::Schedule { command } => match command {
            ScheduleCommand::List { json } => schedule_list_command(&client, &creds, &selection, json),
            ScheduleCommand::Show { id, json } => {
                schedule_show_command(&client, &creds, &selection, &id, None, json)
            }
            ScheduleCommand::Toggle { id, state, json } => schedule_show_command(
                &client,
                &creds,
                &selection,
                &id,
                Some(matches!(state, Toggle::Enable)),
                json,
            ),
            ScheduleCommand::Delete { id } => {
                schedule_delete_command(&client, &creds, &selection, &id)
            }
            ScheduleCommand::Runs { id, limit, json, verify, run } => schedule_runs_command(
                &client, &creds, &selection, &id, limit, json, verify.as_deref(), run.as_deref(),
            ),
            ScheduleCommand::McpServers { id, set, none, json } => match set {
                Some(s) if s.is_empty() => Err(monocle_cli::error::AppError::new(
                    "No servers given to --set: list server ids, or pass --none to clear the schedule's MCP servers.",
                )),
                None if none => schedule_mcp_command(&client, &creds, &selection, &id, Some(&[]), json),
                s => schedule_mcp_command(&client, &creds, &selection, &id, s.as_deref(), json),
            },
            ScheduleCommand::Create { write, mcp_servers, dry_run, json } => {
                schedule_create_command(&client, &creds, &selection, &write, &mcp_servers, dry_run, json)
            }
            ScheduleCommand::Update { id, write, json } => {
                schedule_update_command(&client, &creds, &selection, &id, &write, json)
            }
            ScheduleCommand::Preview { recurrence, dtstart, run_at, timezone, count, json } => {
                schedule_preview_command(
                    &client, &creds, &selection, recurrence.as_deref(), dtstart, run_at, &timezone, count, json,
                )
            }
        },
        Commands::Knowledge { command } => match command {
            KnowledgeCommand::Create { name } => {
                knowledge_create_command(&client, &creds, &selection, &name)
            }
            KnowledgeCommand::Upload { collection, paths } => {
                knowledge_upload_command(&client, &creds, &selection, &collection, &paths)
            }
            KnowledgeCommand::List { collection } => {
                knowledge_list_command(&client, &creds, &selection, collection.as_deref())
            }
            KnowledgeCommand::Update {
                collection,
                name,
                description,
            } => knowledge_update_command(
                &client,
                &creds,
                &selection,
                &collection,
                name.as_deref(),
                description.as_deref(),
            ),
            KnowledgeCommand::Remove { collection, file } => {
                knowledge_remove_command(&client, &creds, &selection, &collection, &file)
            }
            KnowledgeCommand::Replace { collection, path } => {
                knowledge_replace_command(&client, &creds, &selection, &collection, &path)
            }
            KnowledgeCommand::Reprocess {
                collection,
                file,
                force,
            } => {
                knowledge_reprocess_command(&client, &creds, &selection, &collection, &file, force)
            }
            KnowledgeCommand::Content { collection, file } => {
                knowledge_content_command(&client, &creds, &selection, &collection, &file)
            }
            KnowledgeCommand::Ask {
                collection,
                question,
                model,
                json,
            } => knowledge_ask_command(
                &client,
                &creds,
                &selection,
                &collection,
                &question,
                &model,
                json,
            ),
            KnowledgeCommand::Delete { collection } => {
                knowledge_delete_command(&client, &creds, &selection, &collection)
            }
        },
        Commands::WorkspaceModel { command } => match command {
            WorkspaceModelCommand::List => wm::list_command(&client, &creds, &selection),
            WorkspaceModelCommand::Show { id } => {
                wm::show_command(&client, &creds, &selection, &id)
            }
            WorkspaceModelCommand::Create(a) => {
                wm::create_command(&client, &creds, &selection, &a.into())
            }
            WorkspaceModelCommand::Update(a) => {
                wm::update_command(&client, &creds, &selection, &a.into())
            }
            WorkspaceModelCommand::Toggle { id } => {
                wm::toggle_command(&client, &creds, &selection, &id)
            }
            WorkspaceModelCommand::Delete { id } => {
                wm::delete_command(&client, &creds, &selection, &id)
            }
        },
        Commands::Craft { lane, command } => match command {
            CraftCommand::Session { command } => {
                craft_session_command(&client, &creds, &selection, lane, &command)
            }
            CraftCommand::Workspace { command } => {
                craft_workspace_command(&client, &creds, &selection, lane, &command)
            }
            CraftCommand::Agent { command } => {
                craft_agent_command(&client, &creds, &selection, lane, &command)
            }
            CraftCommand::Turn {
                text,
                interactive,
                session,
                continue_last,
                workspace,
                create_workspace,
                agent,
                timeout,
                no_delegation,
                title,
                approve,
                json,
                timings,
                diagnostics_file,
            } => craft_turn_command(
                &client,
                &creds,
                &selection,
                lane,
                no_delegation,
                title,
                TurnOpts {
                    continue_last,
                    last: None,
                    // Only the first turn of a new session says how to continue it.
                    hint_prefix: (!interactive && session.is_none() && !continue_last).then(|| {
                        monocle_cli::commands::craft_turn::hint_prefix(
                            lane == Lane::Sdk,
                            &selection.profile,
                            &selection.environment,
                        )
                    }),
                    interactive,
                    follow_up: false,
                    text: text.unwrap_or_default(),
                    session,
                    workspace,
                    create_workspace,
                    agent,
                    timeout: std::time::Duration::from_secs(timeout),
                    approve,
                    json,
                    poll_every: std::time::Duration::from_secs(2),
                    timings,
                    delegates: !no_delegation,
                    diagnostics_file: monocle_cli::instrument::diagnostics_path(diagnostics_file),
                },
            ),
        },
        Commands::Agent {
            prompt,
            workdir,
            model,
            max_steps,
            session,
            auto_approve,
        } => agent_command(
            &client,
            &creds,
            AgentOptions {
                prompt,
                workdir,
                model,
                max_steps,
                session,
                auto_approve,
            },
        ),
        Commands::Router { command } => match command {
            RouterCommands::Chat(args) => chat_command(&client, &creds, args.into()),
            RouterCommands::Models => model_list_command(&client, &creds),
            RouterCommands::Image { command } => run_image(&client, &creds, command),
            RouterCommands::Audio { command } => run_audio(&client, &creds, command),
        },
        Commands::Upgrade { check } => upgrade_command(&client, check),
        // Rewritten to `Router` by `legacy_alias` above.
        Commands::Models
        | Commands::Audio { .. }
        | Commands::Image { .. }
        | Commands::Model { .. } => {
            unreachable!("legacy alias not rewritten")
        }
    };

    if result.is_ok() {
        update_notice::finish(notice, notice_name, &home, update_notice::DEFAULT_GRACE);
    }

    if let Err(e) = result {
        eprintln!("Error: {e}");
        if diag::was_logged() {
            eprintln!("  (details logged to {})", diag::display_path());
        }
        std::process::exit(1);
    }
}

fn run_audio(client: &Client, creds: &Credentials, command: RouterAudioCommands) -> Result<()> {
    match command {
        RouterAudioCommands::Transcribe(TranscribeArgs {
            file,
            model,
            language,
            prompt,
            response_format,
            temperature,
            filename,
            content_type,
        }) => audio_transcribe_command(
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
        RouterAudioCommands::TranscribeAzure(TranscribeAzureArgs {
            file,
            locale,
            diarization,
            profanity,
            channels,
            definition,
            filename,
            content_type,
        }) => audio_transcribe_azure_command(
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
        RouterAudioCommands::Speak(SpeakArgs {
            text,
            model,
            voice,
            format,
            speed,
            instructions,
            output,
        }) => audio_speech_command(
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
        RouterAudioCommands::SpeakAzure(SpeakAzureArgs {
            ssml,
            format,
            output,
        }) => audio_speech_azure_command(
            client,
            creds,
            ssml.as_deref(),
            AudioSpeechAzureOptions { format, output },
        ),
    }
}

fn run_image(client: &Client, creds: &Credentials, command: ImageCommands) -> Result<()> {
    match command {
        ImageCommands::Generate {
            prompt,
            model,
            size,
            n,
            quality,
            output,
        } => image_generate_command(
            client,
            creds,
            prompt.as_deref(),
            ImageGenerateOptions {
                model,
                size,
                n,
                quality,
                output,
            },
        ),
        ImageCommands::Edit {
            image,
            prompt,
            model,
            mask,
            size,
            n,
            quality,
            output,
        } => image_edit_command(
            client,
            creds,
            &image,
            prompt.as_deref(),
            ImageEditOptions {
                model,
                mask,
                size,
                n,
                quality,
                output,
            },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn craft_lane_parses_in_every_position() {
        let lane = |args: &[&str]| match Cli::parse_from(args).command {
            Commands::Craft { lane, .. } => lane,
            _ => panic!("expected Craft command"),
        };
        for args in [
            &["monocle", "craft", "--lane", "sdk", "turn", "x"][..],
            &["monocle", "craft", "turn", "--lane", "sdk", "x"][..],
            &["monocle", "craft", "session", "list", "--lane", "sdk"][..],
        ] {
            assert_eq!(lane(args), Lane::Sdk, "{args:?}");
        }
        assert_eq!(lane(&["monocle", "craft", "turn", "x"]), Lane::Openworker);
    }

    #[test]
    fn chat_file_flag_repeats_into_vec() {
        let cli = Cli::parse_from(["monocle", "chat", "--file", "a.png", "--file", "b.png"]);
        match cli.command {
            Commands::Chat { args, .. } => {
                assert_eq!(args.file, vec!["a.png".to_string(), "b.png".to_string()]);
            }
            _ => panic!("expected Chat command"),
        }
    }

    #[test]
    fn chat_list_parses_as_list_subcommand() {
        let cli = Cli::parse_from(["monocle", "chat", "list"]);
        match cli.command {
            Commands::Chat { command, .. } => {
                assert!(matches!(command, Some(ChatCommand::List)));
            }
            _ => panic!("expected Chat command"),
        }
    }

    #[test]
    fn chat_content_parses() {
        let cli = Cli::parse_from(["monocle", "chat", "content", "c1"]);
        match cli.command {
            Commands::Chat {
                command: Some(ChatCommand::Content { id, format, .. }),
                ..
            } => {
                assert_eq!(id, "c1");
                assert_eq!(format, ContentFormat::Text);
            }
            _ => panic!("expected chat content"),
        }

        let cli = Cli::parse_from([
            "monocle",
            "chat",
            "content",
            "c1",
            "--format",
            "json",
            "--profile",
            "x",
        ]);
        assert_eq!(cli.profile.as_deref(), Some("x"));
        assert!(matches!(
            cli.command,
            Commands::Chat {
                command: Some(ChatCommand::Content {
                    format: ContentFormat::Json,
                    ..
                }),
                ..
            }
        ));

        let err = Cli::try_parse_from(["monocle", "chat", "content"])
            .err()
            .unwrap();
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
        let err = Cli::try_parse_from(["monocle", "chat", "content", "c1", "--format", "xml"])
            .err()
            .unwrap();
        assert_eq!(err.kind(), clap::error::ErrorKind::InvalidValue);
    }

    #[test]
    fn chat_resume_flag_parses() {
        let cli = Cli::parse_from(["monocle", "chat", "--responses", "--resume", "abc-123"]);
        match cli.command {
            Commands::Chat { command, args } => {
                assert!(command.is_none());
                assert_eq!(args.resume.as_deref(), Some("abc-123"));
            }
            _ => panic!("expected Chat command"),
        }
    }

    use clap::error::ErrorKind;

    fn router(argv: &[&str]) -> RouterCommands {
        match Cli::parse_from(argv).command {
            Commands::Router { command } => command,
            _ => panic!("expected Router command for {argv:?}"),
        }
    }

    fn err_kind(argv: &[&str]) -> ErrorKind {
        match Cli::try_parse_from(argv) {
            Ok(_) => panic!("expected parse error for {argv:?}"),
            Err(e) => e.kind(),
        }
    }

    #[test]
    fn router_chat_parses_router_args_only() {
        let argv = [
            "monocle",
            "router",
            "chat",
            "--model",
            "gpt-4o",
            "--max-tokens",
            "10",
            "--file",
            "a.png",
        ];
        let RouterCommands::Chat(args) = router(&argv) else {
            panic!("expected RouterCommands::Chat");
        };
        let opts = ChatOptions::from(args);
        assert!(!opts.responses);
        assert_eq!(opts.model.as_deref(), Some("gpt-4o"));
        assert_eq!(opts.max_tokens.as_deref(), Some("10"));
        assert_eq!(opts.files, vec!["a.png".to_string()]);
    }

    #[test]
    fn router_chat_rejects_responses_flag() {
        assert_eq!(
            err_kind(&["monocle", "router", "chat", "--responses"]),
            ErrorKind::UnknownArgument
        );
    }

    #[test]
    fn router_models_parses() {
        assert!(matches!(
            router(&["monocle", "router", "models"]),
            RouterCommands::Models
        ));
    }

    #[test]
    fn router_image_generate_and_edit_parse() {
        let gen = [
            "monocle", "router", "image", "generate", "--model", "m", "-o", "o.png",
        ];
        assert!(matches!(
            router(&gen),
            RouterCommands::Image {
                command: ImageCommands::Generate { .. }
            }
        ));
        let edit = [
            "monocle", "router", "image", "edit", "in.png", "--model", "m", "-o", "o.png",
        ];
        assert!(matches!(
            router(&edit),
            RouterCommands::Image {
                command: ImageCommands::Edit { .. }
            }
        ));
    }

    #[test]
    fn router_audio_all_four_parse() {
        use RouterAudioCommands as R;
        let speak = ["monocle", "router", "audio", "speak", "hi", "-o", "o.mp3"];
        assert!(matches!(
            router(&speak),
            RouterCommands::Audio {
                command: R::Speak(_)
            }
        ));
        let speak_azure = [
            "monocle",
            "router",
            "audio",
            "speak-azure",
            "x",
            "-o",
            "o.mp3",
        ];
        assert!(matches!(
            router(&speak_azure),
            RouterCommands::Audio {
                command: R::SpeakAzure(_)
            }
        ));
        assert!(matches!(
            router(&["monocle", "router", "audio", "transcribe"]),
            RouterCommands::Audio {
                command: R::Transcribe(_)
            }
        ));
        assert!(matches!(
            router(&["monocle", "router", "audio", "transcribe-azure"]),
            RouterCommands::Audio {
                command: R::TranscribeAzure(_)
            }
        ));
    }

    #[test]
    fn router_audio_speech_is_not_a_router_name() {
        assert_eq!(
            err_kind(&["monocle", "router", "audio", "speech"]),
            ErrorKind::InvalidSubcommand
        );
    }

    fn alias(argv: &[&str]) -> (String, RouterCommands) {
        match legacy_alias(Cli::parse_from(argv).command) {
            Ok(mapped) => mapped,
            Err(_) => panic!("expected {argv:?} to be a legacy alias"),
        }
    }

    #[test]
    fn legacy_alias_maps_every_root_form() {
        use RouterAudioCommands as R;
        let w = |old: &str, new: &str| {
            format!("Warning: `monocle {old}` is deprecated. Use `monocle {new}` instead.")
        };

        let (warning, rc) = alias(&["monocle", "models"]);
        assert_eq!(warning, w("models", "router models"));
        assert!(matches!(rc, RouterCommands::Models));

        let (warning, rc) = alias(&[
            "monocle",
            "audio",
            "transcribe",
            "a.wav",
            "--language",
            "ko",
        ]);
        assert_eq!(warning, w("audio transcribe", "router audio transcribe"));
        let RouterCommands::Audio {
            command: R::Transcribe(a),
        } = rc
        else {
            panic!("expected Transcribe");
        };
        assert_eq!(a.file.as_deref(), Some("a.wav"));
        assert_eq!(a.language.as_deref(), Some("ko"));

        let (warning, rc) = alias(&["monocle", "audio", "transcribe-azure", "--diarization"]);
        assert_eq!(
            warning,
            w("audio transcribe-azure", "router audio transcribe-azure")
        );
        let RouterCommands::Audio {
            command: R::TranscribeAzure(a),
        } = rc
        else {
            panic!("expected TranscribeAzure");
        };
        assert!(a.diarization);

        let argv = [
            "monocle", "audio", "speech", "hi", "-o", "o.mp3", "--voice", "nova",
        ];
        let (warning, rc) = alias(&argv);
        assert_eq!(warning, w("audio speech", "router audio speak"));
        let RouterCommands::Audio {
            command: R::Speak(a),
        } = rc
        else {
            panic!("expected Speak");
        };
        assert_eq!(a.text.as_deref(), Some("hi"));
        assert_eq!(a.output.as_deref(), Some("o.mp3"));
        assert_eq!(a.voice.as_deref(), Some("nova"));

        let (warning, rc) = alias(&["monocle", "audio", "speech-azure", "x", "-o", "o.mp3"]);
        assert_eq!(warning, w("audio speech-azure", "router audio speak-azure"));
        let RouterCommands::Audio {
            command: R::SpeakAzure(a),
        } = rc
        else {
            panic!("expected SpeakAzure");
        };
        assert_eq!(a.ssml.as_deref(), Some("x"));

        let argv = [
            "monocle", "image", "generate", "p", "--model", "m", "-o", "o.png",
        ];
        let (warning, rc) = alias(&argv);
        assert_eq!(warning, w("image generate", "router image generate"));
        let RouterCommands::Image {
            command: ImageCommands::Generate { model, output, .. },
        } = rc
        else {
            panic!("expected Generate");
        };
        assert_eq!((model.as_str(), output.as_str()), ("m", "o.png"));

        let argv = [
            "monocle", "image", "edit", "in.png", "--model", "m", "-o", "o.png",
        ];
        let (warning, rc) = alias(&argv);
        assert_eq!(warning, w("image edit", "router image edit"));
        let RouterCommands::Image {
            command: ImageCommands::Edit { image, .. },
        } = rc
        else {
            panic!("expected Edit");
        };
        assert_eq!(image, "in.png");

        let (warning, rc) = alias(&["monocle", "model", "chat", "--model", "gpt-4o"]);
        assert_eq!(warning, w("model chat", "router chat"));
        let RouterCommands::Chat(args) = rc else {
            panic!("expected Chat");
        };
        assert_eq!(args.model, "gpt-4o");
    }

    #[test]
    fn legacy_alias_passes_through_non_aliases() {
        for argv in [
            &["monocle", "router", "models"][..],
            &["monocle", "chat"],
            &["monocle", "chat", "list"],
            &["monocle", "token"],
        ] {
            assert!(
                legacy_alias(Cli::parse_from(argv).command).is_err(),
                "{argv:?} must not be an alias"
            );
        }
    }

    #[test]
    fn model_list_is_removed() {
        assert_eq!(
            err_kind(&["monocle", "model", "list"]),
            ErrorKind::InvalidSubcommand
        );
    }

    #[test]
    fn handoff_is_removed() {
        assert_eq!(
            err_kind(&["monocle", "handoff"]),
            ErrorKind::InvalidSubcommand
        );
        assert!(Cli::try_parse_from(["monocle", "handoff", "abc", "--yes"]).is_err());
    }

    #[test]
    fn profile_flag_parses_in_any_position() {
        for argv in [
            &["monocle", "--profile", "x", "router", "models"][..],
            &["monocle", "router", "models", "--profile", "x"],
            &[
                "monocle",
                "router",
                "audio",
                "speak",
                "hi",
                "-o",
                "o",
                "--profile",
                "x",
            ],
            &["monocle", "models", "--profile", "x"],
        ] {
            assert_eq!(
                Cli::parse_from(argv).profile.as_deref(),
                Some("x"),
                "{argv:?}"
            );
        }
        let cli = Cli::parse_from(["monocle", "router", "chat", "--env", "stg"]);
        assert_eq!(cli.env.as_deref(), Some("stg"));
    }

    fn chat_opts(argv: &[&str]) -> ChatOptions {
        match Cli::parse_from(argv).command {
            Commands::Chat { args, .. } => ChatOptions::from(args),
            _ => panic!("expected Chat command for {argv:?}"),
        }
    }

    // Was S1a's `chat_unchanged_in_s1a` (asserted `responses == false`);
    // flipped in 1b: plain `chat` is hosted Chat.
    #[test]
    fn chat_is_hosted_by_default() {
        assert!(chat_opts(&["monocle", "chat"]).responses);
    }

    #[test]
    fn chat_responses_flag_is_hidden_noop() {
        assert!(chat_opts(&["monocle", "chat", "--responses"]).responses);
        let opts = chat_opts(&[
            "monocle",
            "chat",
            "--responses",
            "--resume",
            "abc",
            "--tool-ids",
            "a,b",
        ]);
        assert!(opts.responses);
        assert_eq!(opts.resume.as_deref(), Some("abc"));
        assert_eq!(opts.tool_ids, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn hyphen_leading_profile_and_session_parse_in_equals_form() {
        let cli = Cli::try_parse_from([
            "monocle",
            "craft",
            "turn",
            "--profile=-work",
            "--session=-id",
            "hi",
        ])
        .unwrap();
        assert_eq!(cli.profile.as_deref(), Some("-work"));
    }

    #[test]
    fn claude_split_finds_subcommand_after_globals_only() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(claude_split(&v(&["m", "claude", "-v"])), Some(1));
        assert_eq!(
            claude_split(&v(&["m", "-v", "--profile", "claude", "claude"])),
            Some(4)
        );
        assert_eq!(claude_split(&v(&["m", "chat", "claude"])), None);
    }

    #[test]
    fn chat_router_flags_still_parse() {
        let opts = chat_opts(&[
            "monocle",
            "chat",
            "--system-prompt",
            "s",
            "--system-prompt-file",
            "f",
            "--max-tokens",
            "5",
        ]);
        assert!(opts.responses);
        assert_eq!(opts.system_prompt.as_deref(), Some("s"));
        assert_eq!(opts.system_prompt_file.as_deref(), Some("f"));
        assert_eq!(opts.max_tokens.as_deref(), Some("5"));
    }

    #[test]
    fn router_chat_still_router() {
        let RouterCommands::Chat(args) = router(&["monocle", "router", "chat"]) else {
            panic!("expected RouterCommands::Chat");
        };
        assert!(!ChatOptions::from(args).responses);
    }

    #[test]
    fn chat_help_hides_responses_and_router_flags() {
        use clap::CommandFactory;
        let help = Cli::command()
            .find_subcommand_mut("chat")
            .expect("chat subcommand")
            .render_help()
            .to_string();
        for hidden in ["--responses", "--system-prompt", "--max-tokens"] {
            assert!(!help.contains(hidden), "{hidden} in chat help:\n{help}");
        }
        assert!(help.contains("--resume"), "{help}");
    }
}
