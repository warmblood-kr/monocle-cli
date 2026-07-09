//! Library surface for the `monocle` CLI. The binary (`src/main.rs`) is a thin
//! clap front-end over these modules; tests import them directly.

pub mod acp;
pub mod agent;
pub mod attachment;
pub mod audio_io;
pub mod auth;
pub mod colors;
pub mod commands;
pub mod credentials;
pub mod diag;
pub mod endpoints;
pub mod error;
pub mod net;
pub mod oidc;
pub mod origin;
pub mod refresh;
pub mod util;
