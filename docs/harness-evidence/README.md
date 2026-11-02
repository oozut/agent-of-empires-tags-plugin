# Harness evidence

One file per harness, written by `tools/probe-harness.sh <agent>`: what this session's tag set
looked like on the way out (`minted_values`), how the harness was pointed at its per-session
config (`session_override`), and every delivery the sink actually saw.

The probe is self-contained — throwaway `HOME`, the harness binary by absolute path, a local
sink instead of a model endpoint — so these files can be regenerated on any machine that has the
harness installed. A harness is `verified` in `adapters.toml` when its file here shows
`"delivered": true` and the delivery is the one the row claims.

| Agent | Session scope | Delivered |
|---|---|---|
| `pi` | `PI_CODING_AGENT_DIR` (overlay dir) | header + `samplingParams` body field |
| `claude` | `ANTHROPIC_CUSTOM_HEADERS` (env only) | header |
| `codex` | `CODEX_HOME` (overlay dir, TOML) | header |
| `opencode` | `OPENCODE_CONFIG` (overlay file) | header |
