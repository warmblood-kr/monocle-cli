# Monocle CLI profiles and hosted Chat handoff

Status: implementation design for `warmblood-kr/monocle#1071`, #1072, #1073, and credential compatibility tracked in #340. Target is the current Rust `warmblood-kr/monocle-cli` `devel` branch at `e4856d8` (release 1.6.0). The repository evidence is `Cargo.toml`, `src/main.rs`, and `src/credentials.rs`; the CLI is a Rust standalone binary. Issue #340 is the existing credential-safety work in this same repository and is part of this delivery, not a separate runtime.

## Product model and selection

A **profile** is a named Monocle connection slot (`warmblood`, `warmblood-stg`). Its **environment** is the default deployment target for that connection (`prod`, `stg`, `local`). These remain separate values; each profile remembers its environment, tenant and credentials. The environment selects a credential slot as well as the endpoint and is displayed with the profile.

Profile precedence is explicit root or subcommand `--profile`, then `MONOCLE_PROFILE`, then `default_profile` in config, then `default`. Environment precedence is explicit `--env`, saved environment for the chosen profile, inference from its legacy credentials when upgrading an existing install, then `prod`. `--env` selects an environment slot; it never reuses credentials from a different environment. Missing profile/credential, invalid names, malformed profile config, or invalid environment fail with a specific error and never silently fall through.

`monocle profile list`, `show [NAME]`, and `use NAME` support discovery and default selection. `use` writes the default profile only; a profile's environment changes only through a successful login using that profile and `--env` (or its already-saved environment when omitted). `login --force` is not needed to change environments because credentials are stored by profile and environment rather than overwritten in place.

## Config and credentials

Non-secret settings remain in `~/.monocle/config.json`, extending the existing config with serde defaults so `diag_always_on` and older config remain readable:

```json
{
  "diag_always_on": false,
  "default_profile": "warmblood",
  "profiles": {
    "warmblood": { "environment": "prod" },
    "warmblood-stg": { "environment": "stg" }
  }
}
```

The current TypeScript drop-in contract remains unchanged: the legacy `default` profile's credentials for its inferred/current environment continue to use `~/.monocle/credentials.json`, preserving JSON fields/order and mode `0600`. No first-use move or reauthentication is required. Other profile/environment pairs use `~/.monocle/profiles/<profile>/credentials.<env>.json`; directories are `0700`, credential files `0600`. For the `default` profile, a different environment uses the same profile directory and environment-specific filename, leaving the legacy file untouched. On a default-profile read with no environment file, the legacy file is eligible only when the requested environment matches the environment inferred from `tenant_domain` (staging prefix, localhost, otherwise prod). No named profile falls back to the legacy file. Backups and migration are unnecessary for this additive layout; removing the legacy read path is explicitly out of scope.

Each credentials record retains the existing schema. Profile metadata stores no tokens. Token refresh reads and writes only the selected profile/environment path. `profile show`, status, setup, Claude Code, and handoff show or pin both selected values.

## Command coverage and isolation

Root `--profile` is global; auth-bearing commands also accept `--profile` and `--env` after the command. Resolution applies consistently to login, token, status, setup, Claude, chat/list, handoff, models, agent, ACP, audio, and image requests. `unset` remains global because it removes shared Claude Code settings, not credentials. `token` still writes only the access token to stdout. Claude Code's helper pins the resolved profile and environment so a later shell change cannot move a running integration to another tenant.

Every API call uses the selected credentials' bearer token and tenant/router origin. Profile and environment resolve a single file before refresh; there is no cross-profile or cross-environment fallback. Existing default scripts and `monocle token` keep reading `credentials.json` when profile/environment are omitted. Credential JSON shape and ordering are not changed.

## Read-only hosted Chat handoff

The primary source is existing server-side Monocle Chat history, not the local `monocle chat` process. This repository already has `src/jarvice_chats.rs` for authenticated `GET /api/v1/chats/` and `GET /api/v1/chats/{id}`; the list and detail requests use the selected session's tenant. The implementation reuses that client and active ancestry (`currentId`/`parentId`) extraction. Requests are reads only. The user selects exactly one authorized thread; there is no automatic bulk import and no write, replay, archive, or edit request.

`monocle handoff` lists thread metadata, fetches the chosen thread, then shows profile, environment, tenant, title, time and message count for confirmation. Interactive mode prompts for one selection and confirmation. Non-interactive mode requires an explicit thread id and confirmation flag. Only the active message chain is passed. Text is marked as untrusted context; non-text attachments are identified as omitted. Enforce a 1 MiB transcript bound and fail without truncation.

The transcript is held in an automatically removed private temporary directory/file (directory `0700`, file `0600`) for the Claude Code process lifetime. The child gets a prompt with the transcript path, not chat contents in argv, logs, or shell history. The file is deleted after Claude exits or launch fails. API errors are sanitized to status/reason without response bodies that could echo chat material. A denied/missing thread, refusal, size limit, or missing Claude executable fails without mutating the hosted thread.

## First-party product scope and sequence

The Rust CLI already provides OIDC login, tokens, Claude Code setup/launch, direct router chat and model listing, image/audio commands, agent mode and ACP. This epic ships profiles plus hosted Chat handoff as the center-first slice. Follow-on work should remain separate: (1) group direct router calls under `monocle router` while keeping current command aliases; (2) scope hosted Chat history, MCP, and custom model capabilities; (3) independently scope scheduled tasks, file drive, and Craft; (4) treat Admin as security evaluation only until least-privilege authorization boundaries are proven. This sequence is a recommendation, not an expansion of #1071.

## Validation and release boundary

The normal release channel is GitHub Releases with prebuilt binaries (`install.sh`/`install.ps1`). There is no staging deployment for this CLI. Validate with `cargo fmt --check`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, release build/package checks, and PR CI. No staging or production deployment is part of this work.
