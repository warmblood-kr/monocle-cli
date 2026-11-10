# Experimental core tool workflow

`monocle agent --experimental-core-tool-workflow --session NAME` opts into the bounded serial shared-core tool adapter. The model-only flag conflicts with this flag; the default native lane remains selected otherwise. The core snapshot is vendored for reproducible review; history APIs are present but no history consumer is enabled because the canonical envelope is incompatible with the native chat-completions wire contract.

A pending sidecar journal is synced before tool dispatch and retained after uncertain failure. All named-session agent routes refuse an existing marker before provider/auth construction. Reconcile the tool effect, journal and session before manually removing a pending marker; the CLI does not replay or automatically clear uncertainty. A successful complete session append permits marker removal.

Focused local verification: actual loopback CLI smoke (two nonstream model requests, read_file, preserved assistant tool-call text, ordered session append and marker cleanup); 22 lifecycle tests; auth fixture; provider-error-after-effect, post-effect cancellation/reopen, marker-write failure before dispatch; default/model-only/workflow reopen provider-zero and metadata-error fail-closed checks. Formatting, diff and exact file hash checks passed. Component-only tool/history and assembly suites were separately independently reviewed.

The broad library run had 308 passes and one forced-color assertion failure, reproduced on the baseline. A later shared-target filtered attempt hit stale vendored artifacts; it is not a complete broad-suite pass. Final focused runs used isolated offline cache/target directories. Live provider credentials, deployment, history adoption and overall RFC completion were not tested.
