#!/usr/bin/env bash
# Verify one harness end to end: does this session's tag set actually arrive at
# the far side of an LLM request?
#
# The probe is deliberately hermetic and repeatable:
#   * its own HOME and XDG_CONFIG_HOME under /tmp, so no user config is read or
#     written — the plugin's per-session overlay is the only thing exercised;
#   * the harness binary is called by absolute path (a user-prefix install in
#     ~/.local/bin works, and need not be visible to `aoe agents`);
#   * the destination is tools/header-sink.py, so the check needs no API key and
#     no model: the binary is expected to fail, and what it *sent* is the result;
#   * the outcome is recorded as JSON in docs/harness-evidence/<agent>.json, so a
#     later run can be compared instead of re-argued.
#
# Usage: tools/probe-harness.sh <agent> [--cli PATH] [--bin PATH] [--sink PORT]
#                                  [--extra-settings FILE]
#        tools/probe-harness.sh --list
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EVIDENCE="$ROOT/docs/harness-evidence"
HOST_AGENTS=(pi claude codex opencode)

die() { printf 'probe: %s\n' "$*" >&2; exit 2; }
note() { printf 'probe: %s\n' "$*" >&2; }

usage() {
    sed -n '2,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

agent=""
cli="${AOE_LLM_TAGS_CLI:-}"
bin="${AOE_PROBE_BIN:-}"
sink_port="${AOE_PROBE_PORT:-8899}"
extra_settings=""
while (($#)); do
    case "$1" in
        --list) printf '%s\n' "${HOST_AGENTS[@]}"; exit 0 ;;
        --cli) cli="$2"; shift 2 ;;
        --bin) bin="$2"; shift 2 ;;
        --sink) sink_port="$2"; shift 2 ;;
        --extra-settings) extra_settings="$2"; shift 2 ;;
        -h | --help) usage 0 ;;
        -*) die "unknown option $1" ;;
        *) agent="$1"; shift ;;
    esac
done
[[ -n "$agent" ]] || usage 2

# The CLI that the host hooks run: the installed plugin binary, else the debug
# build in this tree, so the probe works before and after `aoe plugin update`.
if [[ -z "$cli" ]]; then
    for candidate in \
        "$HOME/.config/agent-of-empires/plugins/dev.oozut.llm-tags/bin/aoe-llm-tags" \
        "$ROOT/target/debug/aoe-llm-tags" \
        "$ROOT/target/release/aoe-llm-tags"; do
        [[ -x "$candidate" ]] && cli="$candidate" && break
    done
fi
[[ -n "$cli" && -x "$cli" ]] || die "no aoe-llm-tags binary; pass --cli <path>"

# The harness binary: explicit, else a user-prefix install, else PATH.
if [[ -z "$bin" ]]; then
    for candidate in "$HOME/.local/bin/$agent" "/usr/local/bin/$agent" "/usr/bin/$agent"; do
        [[ -x "$candidate" ]] && bin="$candidate" && break
    done
    bin="${bin:-$(command -v "$agent" 2>/dev/null)}"
fi

work="$(mktemp -d "/tmp/aoe-probe-$agent-XXXXXX")"
trap 'rm -rf "$work"' EXIT
export HOME="$work/home"
export XDG_CONFIG_HOME="$work/home/.config"
mkdir -p "$HOME" "$XDG_CONFIG_HOME" "$work/bin"

session="probe-$agent-$$"
project="$work/project"
mkdir -p "$project"

# A fake `aoe` so the resolver sees exactly the session the probe claims to be.
fake_aoe="$work/bin/aoe"
cat >"$fake_aoe" <<EOF
#!/bin/sh
case "\$1" in
  session) echo '{"id":"$session","title":"probe","path":"$project","group":"","tool":"$agent","profile":"main"}' ;;
  list) echo '{"sessions":[{"id":"$session"}]}' ;;
  *) echo '{}' ;;
esac
EOF
chmod +x "$fake_aoe"

# The probe's own plugin config: one static tag so a delivery is unambiguous, and
# the caller can add targets (a body field, a query parameter) to test those.
mkdir -p "$XDG_CONFIG_HOME/agent-of-empires"
{
    printf '%s\n' '[plugins."dev.oozut.llm-tags".settings]' 'tags = ["team:core"]'
    if [[ -n "$extra_settings" ]]; then cat "$extra_settings"; fi
} >"$XDG_CONFIG_HOME/agent-of-empires/config.toml"

sink_log="$work/sink.jsonl"
python3 "$ROOT/tools/header-sink.py" "$sink_port" "$sink_log" --reset >"$work/sink.out" 2>&1 &
sink_pid=$!
cleanup() { kill "$sink_pid" 2>/dev/null; }
keep() { [[ -n "${AOE_PROBE_KEEP:-}" ]] && { note "kept $work"; return 1; }; return 0; }
trap 'cleanup; keep || rm -rf "$work"' EXIT
for _ in $(seq 50); do
    grep -q listening "$work/sink.out" 2>/dev/null && break
    sleep 0.1
done

base="http://127.0.0.1:$sink_port"

# Each recipe writes the fixture config the harness's adapter row points at, so
# the probe reads the real path from adapters.toml instead of guessing.
case "$agent" in
pi)
    mkdir -p "$HOME/.pi/agent"
    cat >"$HOME/.pi/agent/models.json" <<EOF
{"providers":{"probe":{"baseUrl":"$base/v1","api":"openai-completions","apiKey":"probe","models":[{"id":"probe-model","name":"probe-model"}]}}}
EOF
    run=("$bin" -p "hi")
    extra=(PI_PROVIDER=probe PI_MODEL=probe-model)
    ;;
claude)
    # claude carries the header in ANTHROPIC_CUSTOM_HEADERS, so the fixture is
    # only what an ordinary user already has: a settings file and a login blob.
    mkdir -p "$HOME/.claude"
    printf '{"env":{}}\n' >"$HOME/.claude/settings.json"
    printf '{"probe":{"accessToken":"probe"}}\n' >"$HOME/.claude/.credentials.json"
    run=("$bin" -p "hi")
    extra=(ANTHROPIC_BASE_URL="$base" ANTHROPIC_AUTH_TOKEN=probe
           ANTHROPIC_MODEL=probe-model ANTHROPIC_SMALL_FAST_MODEL=probe-model
           CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1)
    ;;
codex)
    mkdir -p "$HOME/.codex"
    cat >"$HOME/.codex/config.toml" <<EOF
model = "probe-model"
model_provider = "probe"

[model_providers.probe]
name = "probe"
base_url = "$base/v1"
env_key = "PROBE_KEY"
wire_api = "responses"

# The reference aoe cannot generate for codex: the config names the variable,
# the hook exports it, so nothing per-session is written anywhere.
[model_providers.probe.env_http_headers]
"X-AoE-Tags" = "AOE_LLM_TAGS"
EOF
    run=("$bin" exec --skip-git-repo-check "hi")
    extra=(PROBE_KEY=probe OPENAI_API_KEY=probe)
    ;;
opencode)
    mkdir -p "$XDG_CONFIG_HOME/opencode"
    cat >"$XDG_CONFIG_HOME/opencode/opencode.json" <<EOF
{"provider":{"probe":{"npm":"@ai-sdk/openai-compatible","name":"probe","options":{"baseURL":"$base/v1","apiKey":"probe"},"models":{"probe-model":{"name":"probe-model"}}}}}
EOF
    run=("$bin" run "hi" --model probe/probe-model)
    extra=()
    ;;
*) die "no probe recipe for $agent (known: ${HOST_AGENTS[*]})" ;;
esac

# The hook is what a launch actually runs: its stdout is the environment aoe
# injects, including the per-session config override.
hook_out="$work/hook.env"
env AOE_BIN="$fake_aoe" AOE_SESSION_ID="$session" AOE_TOOL="$agent" \
    AOE_PROJECT_PATH="$project" AOE_PROFILE=main \
    "$cli" hook >"$hook_out" 2>"$work/hook.err"

started_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
# One argv element per assignment: a value with a space stays one value.
mapfile -t assignments < <(awk 'index($0, "=") > 1' "$hook_out" 2>/dev/null || true)
if [[ ${#assignments[@]} -eq 0 ]]; then
    while IFS= read -r line; do
        [[ "$line" == *=* ]] && assignments+=("$line")
    done <"$hook_out"
fi
timeout 120 env "${assignments[@]}" "${extra[@]}" "${run[@]}" >"$work/run.out" 2>&1
run_code=$?
sleep 0.3
cleanup

deliveries=0
if [[ -s "$sink_log" ]]; then
    deliveries=$(python3 - "$sink_log" <<'PY'
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1]) if line.strip()]
print(len(rows))
PY
)
fi

mkdir -p "$EVIDENCE"
python3 - "$sink_log" "$EVIDENCE/$agent.json" "$agent" "$bin" "$cli" "$started_at" "$run_code" "$hook_out" <<'PY'
import json, sys, pathlib

log, out, agent, binary, cli, started_at, run_code, hook_path = sys.argv[1:9]
rows = []
if pathlib.Path(log).exists():
    rows = [json.loads(line) for line in open(log) if line.strip()]
timestamp = str(rows[0].get("ts") or started_at) if rows else started_at

hook = [
    line.split("=", 1)
    for line in pathlib.Path(hook_path).read_text().splitlines()
    if "=" in line
]
tags = [value for name, value in hook if name.startswith("AOE_") and value]
overrides = {name: value for name, value in hook if not name.startswith("AOE_")}

def matches(value, needle):
    return needle and needle in value

evidence = {
    "agent": agent,
    "binary": binary,
    "cli": cli,
    "checked_at": timestamp,
    "exit_code": int(run_code),
    "session_override": overrides,
    "minted_values": tags,
    "requests": len(rows),
    "deliveries": [],
    "delivered": False,
}
# A delivery counts when the far side can read the tag set back. The default
# format is one JSON object, so a header/query/body value is parsed and compared
# field by field; a flat `csv` value still matches as a substring.
import urllib.parse

wanted = []
for tag in tags:
    try:
        wanted.append(json.loads(tag))
    except ValueError:
        wanted.append(tag)


def holds(candidate):
    """The minted tag set, read out of `candidate`, or None."""
    if not candidate:
        return None
    parsed = candidate
    if isinstance(candidate, str):
        try:
            parsed = json.loads(candidate)
        except ValueError:
            parsed = None
    for want in wanted:
        if isinstance(want, dict) and isinstance(parsed, dict):
            if all(parsed.get(key) == value for key, value in want.items()):
                return parsed
        elif isinstance(want, str) and want and want in str(candidate):
            return candidate
    return None


def find(candidate):
    found = holds(candidate)
    if found is not None:
        return found
    if isinstance(candidate, dict):
        for item in candidate.values():
            found = find(item)
            if found is not None:
                return found
    if isinstance(candidate, list):
        for item in candidate:
            found = find(item)
            if found is not None:
                return found
    return None


for row in rows:
    for name, value in (row.get("headers") or {}).items():
        found = holds(str(value))
        if found is not None:
            evidence["deliveries"].append({"where": "header", "name": name, "value": found})
    found = find(row.get("body") or {})
    if found is not None:
        evidence["deliveries"].append({"where": "body", "value": found})
    query = urllib.parse.parse_qs(urllib.parse.urlsplit(row.get("path", "")).query)
    for name, values in query.items():
        for value in values:
            found = holds(urllib.parse.unquote(value))
            if found is not None:
                evidence["deliveries"].append({"where": "query", "name": name, "value": found})
                break
evidence["delivered"] = bool(evidence["deliveries"])
pathlib.Path(out).write_text(json.dumps(evidence, indent=2) + "\n")
print(json.dumps(evidence, indent=2))
sys.exit(0 if evidence["delivered"] else 1)
PY
status=$?

if [[ $status -eq 0 ]]; then
    note "$agent: delivered (evidence docs/harness-evidence/$agent.json)"
else
    note "$agent: nothing delivered; hook said:"
    sed 's/^/  /' "$hook_out" >&2
    sed 's/^/  /' "$work/hook.err" >&2
    note "harness output ($work/run.out):"
    tail -5 "$work/run.out" | sed 's/^/  /' >&2
fi
exit $status
