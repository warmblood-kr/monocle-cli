# Profile and hosted Chat handoff design

Status: implementation design for epic #1071 and stories #1072/#1073, targeting the TypeScript `warmblood-kr/monocle-cli` repository and its `devel` branch.

## Product model and repository boundary

A **profile** is a named saved connection and credential slot, for example `warmblood` or `warmblood-stg`. Its **environment** is the Monocle deployment it targets: `prod`, `stg`, or `local`. Profile names and environment values are separate; `--env stg` remains an environment selector and is not a profile alias. Each profile stores its own environment and credentials.

Epic #1071 lives in `warmblood-kr/monocle`; this implementation belongs in the linked TypeScript `warmblood-kr/monocle-cli` repo. The TypeScript repository evidence is `package.json` (`@warmblood/monocle-cli`, Node >=18, Commander) plus `src/cli.ts` and `src/commands/*`; linked issue #340 instead cites Rust paths and the Rust CLI's single-file overwrite hazard. This TypeScript change will not claim to fix that Rust implementation or create a duplicate security issue. It will maintain the existing TS credential file contract, isolate named profiles, and retain restrictive permissions.

## Selection and precedence

Support both the global spelling from the story and a command-local spelling:

1. Explicit `--profile NAME` (including `monocle --profile NAME <command>`).
2. `MONOCLE_PROFILE`.
3. `default_profile` in `~/.monocle/config.json`.
4. Implicit `default`.

An invalid name, malformed config, or unlogged-in profile is an error. Never fall through to another profile. Profile names are restricted to letters, digits, dot, underscore, and hyphen before use as a path or helper argument.

`login --env ENV` selects the OIDC deployment for that login and records that environment on the selected profile after successful authentication. If `--env` is omitted while logging into an existing named profile, reuse the environment saved for that profile; for a new profile use `prod`. Existing credentials without environment metadata infer `stg` from a `stg-` tenant domain prefix, `local` from localhost, and otherwise `prod`. A named profile's saved environment is the default for login and is displayed by `profile show` and `status`. If an explicit environment differs from the selected profile's current environment, login fails before network access; `--force` is the deliberate replacement escape hatch.

## Config and credential storage

Non-secret settings live at `~/.monocle/config.json`:

```json
{
  "version": 1,
  "default_profile": "warmblood",
  "profiles": {
    "warmblood": { "environment": "prod" },
    "warmblood-stg": { "environment": "stg" }
  }
}
```

The default profile keeps using `~/.monocle/credentials.json`, with the existing JSON schema, so existing scripts and old installations continue to work without reauthentication or a forced move. Named profiles use `~/.monocle/profiles/<name>/credentials.json`. Credentials are written with mode `0600`; private profile directories use `0700`. Config contains no tokens.

There is no destructive migration. The legacy file is the `default` profile. First use leaves it in place. Named login writes only that profile. No automatic relocation, aliasing, or fallback from named profiles to legacy credentials occurs. This avoids invalidating deployed `monocle token` helpers and honors #340's backward-compatibility constraint without claiming its Rust-side migration is complete.

## First-party CLI roadmap boundary

The current package already supports OIDC login, token/status/setup/Claude Code helpers, and direct Monocle router model listing and chat (`model list` and `model chat`). Profiles and hosted Chat history handoff are the center-first slice in #1071. The current direct router wrapper remains available for compatibility while the product surface is organized as a first-party application client.

Keep subsequent product work in separate candidate epics/stories, in this order:

1. **Router command organization:** place the legacy direct API wrapper under `monocle router` and retain existing commands as backward-compatible aliases. This is a CLI structure change; it does not add router capabilities by itself.
2. **Chat history, MCP, and custom models:** build on the hosted Chat API and define supported MCP and custom model workflows with their own authorization and compatibility reviews.
3. **Scheduled tasks, file drive, and Craft:** scope each integration independently after its APIs, tenant boundaries, and user consent flows are known.
4. **Admin:** security-evaluation work only until safe authorization boundaries and least-privilege operations are proven; do not include administrative capabilities in the current epic.

These are recommendations for later issue creation and are not implementation requirements or a scope expansion for #1071. No issues for the later candidates are created by this PR.

## Command coverage

The global selector is available before any subcommand, and auth-bearing commands also accept it after the command for shell convenience: `login`, `token`, `status`, `setup`, `model list`, `model chat`, `claude`, and `handoff`. Add `profile list` and `profile show [name]` for discovery and environment confirmation. `profile use NAME` sets the default. `unset` remains global because it removes global Claude Code routing; it does not read or delete credentials.

`token` resolves the same profile as other commands and writes only its access token to stdout. Claude Code helper configuration always embeds the resolved profile (`monocle token --profile NAME`), so a later shell environment change cannot switch tenants mid-session. `setup` writes a profile-scoped helper and the selected router URL to the existing global Claude Code config. `monocle claude` remains invocation-scoped and does not mutate global settings.

## Credential and tenant isolation

Read, refresh, write, and delete operations use one selected profile store. Refresh writes back to that same credential file. The environment label is shown for operator clarity; API requests use the selected profile's tenant and token. A profile mismatch or missing file fails closed. The sole compatibility exception is `default`, which maps directly to the existing credentials file.

Never log bearer tokens or chat contents. Diagnostics for hosted Chat requests show only HTTP status and a sanitized reason, not a server body that might echo request material. Profile names are validated. Chat listing/detail APIs use the selected profile's bearer token and can return only chats authorized to that identity.

## Read-only hosted Chat handoff

The feature is an explicit `monocle handoff` workflow over the hosted Chat API, not a handoff of the local `monocle model chat` REPL. The hosted service provides `GET /api/v1/chats/` for the signed-in user's list and `GET /api/v1/chats/{id}` for an authorized chat. The detail route enforces owner or project-membership access and is read-only. Use the selected profile's tenant origin and refreshed Stark bearer token.

On an interactive terminal, list chats with title and update time, let the user select exactly one, fetch its detail, then show profile, environment, tenant, chat title/time, and message count before asking for confirmation. On non-interactive input, require an explicit chat ID and confirmation flag; never select all or silently hand off. Extract only the chosen chat's active message chain. Treat chat contents as untrusted reference material. Mark non-text attachments as omitted. The handoff is a temporary UTF-8 Markdown file created with mode `0600`; Claude Code receives a prompt containing only that file path. Keep the file until Claude Code exits, then delete it in `finally`. Do not persist a chat export in the project, shell history, or diagnostics. Bound the transcript size (1 MiB) and fail without truncation if exceeded.

Claude Code runs in the current working directory, with the same profile-scoped token helper and selected Monocle router URL. Failure to list/read an authorized chat, refusal at confirmation, or missing Claude Code does not change the server chat. The only API calls are reads; the CLI never writes, clones, archives, or edits a hosted chat.

## Minimum delivery and deferred work

Deliver named profile/environment config and path isolation, flag/environment/default precedence, default-profile compatibility, global and local command selection including token/Claude Code, profile list/show, status display, selected-chat list/detail/confirm, temporary read-only transcript handoff, docs, and tests. Defer profile deletion/rename, persistent transcript exports, hosted Chat mutation, and any migration of the separate Rust CLI in #340. No staging or production deployment is included.

## Release and validation path

This repository publishes the npm package `@warmblood/monocle-cli`; `.github/workflows/publish.yml` runs lint, tests, build, and an npm dry run on a GitHub Release, then publishes through the `npm-production` environment. The package has no staging deployment target. Review and CI on the PR, followed by local/CI tests and an npm package dry run, are the available pre-release validation channels. A package install from a preview artifact could be used for manual validation if configured, but this PR does not publish a package or deploy anywhere. A later release requires the normal release decision and must not be treated as a staging rollout.
