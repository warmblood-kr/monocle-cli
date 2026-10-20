//! Library surface for the `monocle` CLI. The binary (`src/main.rs`) is a thin
//! clap front-end over these modules; tests import them directly.

pub mod acp;
pub mod agent;
pub mod attachment;
pub mod audio_io;
pub mod auth;
pub mod colors;
pub mod commands;
pub mod config;
pub mod craft_server;
pub mod craft_state;
pub mod credentials;
pub mod diag;
pub mod endpoints;
pub mod error;
pub mod image_io;
pub mod instrument;
pub mod jarvice_agent;
pub mod jarvice_chats;
pub mod jarvice_knowledge;
pub mod jarvice_mcp;
pub mod jarvice_models;
pub mod jarvice_schedules;
pub mod net;
pub mod oidc;
pub mod origin;
pub mod profiles;
pub mod refresh;
pub mod responses_api;
pub mod socketio;
pub mod trace;
pub mod update_notice;
pub mod util;
pub mod ws;
