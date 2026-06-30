//! Agent subsystem (Path B — native Rust headless agent loop).
//!
//! Design: warmblood-kr/monocle#158 (SDD) · impl: warmblood-kr/monocle-cli#44.
//! This is the §9 step-1 spike: the `providers` abstraction only. The agent loop,
//! tools, permission/sandboxing, session, and ACP surface come later.

pub mod providers;
pub mod runner;
pub mod tools;
