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
#        tools/probe-harness.sh --all          # every installed agent with a recipe
#        tools/probe-harness.sh --list
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EVIDENCE="$ROOT/docs/harness-evidence"
HOST_AGENTS=(pi claude codex opencode hermes gemini qwen droid)

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
all=0
while (($#)); do
    case "$1" in
        --list) printf '%s\n' "${HOST_AGENTS[@]}"; exit 0 ;;
        --all) all=1; shift ;;
        --cli) cli="$2"; shift 2 ;;
        --bin) bin="$2"; shift 2 ;;
        --sink) sink_port="$2"; shift 2 ;;
        --extra-settings) extra_settings="$2"; shift 2 ;;
        -h | --help) usage 0 ;;
        -*) die "unknown option $1" ;;
        *) agent="$1"; shift ;;
    esac
done
# One run per installed harness, each on its own sink port, so one command
# rebuilds every evidence file: an upgrade is re-checked, not re-argued.
if ((all)); then
    status=0
    port="${sink_port}"
    for candidate in "${HOST_AGENTS[@]}"; do
        bin_path=""
        for dir in "$HOME/.local/bin" /usr/local/bin /usr/bin; do
            [[ -x "$dir/$candidate" ]] && bin_path="$dir/$candidate" && break
        done
        bin_path="${bin_path:-$(command -v "$candidate" 2>/dev/null)}"
        if [[ -z "$bin_path" ]]; then
            note "$candidate: not installed here, skipped"
            continue
        fi
        args=("$candidate" --bin "$bin_path" --sink "$port")
        [[ -n "$cli" ]] && args+=(--cli "$cli")
        [[ -n "$extra_settings" ]] && args+=(--extra-settings "$extra_settings")
        "$0" "${args[@]}" || status=1
        port=$((port + 1))
    done
    exit $status
fi
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
hermes)
    # A reference row: the config names the variable, hermes expands `${VAR}`
    # itself, and aoe refuses to move this file (it owns the agent's hooks).
    mkdir -p "$HOME/.hermes"
    cat >"$HOME/.hermes/config.yaml" <<EOF
providers:
  probe:
    base_url: "$base/v1"
    api_key: "probe"
    extra_headers:
      X-AoE-Tags: "\${AOE_LLM_TAGS}"
model: probe-model
provider: probe
EOF
    run=("$bin" -z "hi")
    extra=()
    ;;
qwen)
    mkdir -p "$HOME/.qwen"
    cat >"$HOME/.qwen/settings.json" <<EOF
{"modelProviders":{"openai":[{"id":"probe-model","name":"probe-model","baseUrl":"$base/v1",
  "envKey":"OPENAI_API_KEY","generationConfig":{"customHeaders":{
    "X-Aoe-Tags":"\${AOE_LLM_TAGS}"}}}]},
 "security":{"auth":{"selectedType":"openai"}}}
EOF
    run=("$bin" -m probe-model -p "hi")
    extra=(OPENAI_API_KEY=probe)
    ;;
gemini)
    mkdir -p "$HOME/.gemini"
    # Only the auth choice is written: the endpoint and the header list both ride
    # the launch environment (`GOOGLE_GEMINI_BASE_URL`, `GEMINI_CLI_CUSTOM_HEADERS`).
    cat >"$HOME/.gemini/settings.json" <<EOF
{"security":{"auth":{"selectedType":"gemini-api-key"}}}
EOF
    # gemini splits its header list on `,(name:)`, so a JSON payload is addressed
    # as two headers and the request is dropped. The probe therefore asks for the
    # same payload base64url-encoded — the escape hatch `setup` points at.
    cat >>"$XDG_CONFIG_HOME/agent-of-empires/config.toml" <<'EOF'
[[plugins."dev.oozut.llm-tags".settings.targets]]
kind = "header"
name = "X-Aoe-B64"
template = "{tags_b64}"
EOF
    run=("$bin" -p "hi")
    extra=(GEMINI_API_KEY=probe GOOGLE_GEMINI_BASE_URL="$base" GEMINI_CLI_TRUST_WORKSPACE=true)
    # The generic target set includes the JSON header, whose commas gemini reads
    # as two headers. These two are what gemini can actually carry.
    targets_block='[[plugins."dev.oozut.llm-tags".settings.targets]]
kind = "header"
name = "X-Aoe-B64"
template = "{tags_b64}"
[[plugins."dev.oozut.llm-tags".settings.targets]]
kind = "header"
name = "X-Aoe-Session"
template = "{session_id}"'
    ;;
droid)
    # droid validates a Factory account before it sends anything, so this recipe
    # records what the binary's own config does — `extraHeaders` with `${VAR}`
    # expansion — and reports `blocked-by-auth` rather than pretending the
    # channel failed.
    mkdir -p "$HOME/.factory"
    cat >"$HOME/.factory/settings.json" <<EOF
{"customModels":[{"model":"probe-model","provider":"openai","baseUrl":"$base/v1","apiKey":"probe",
  "extraHeaders":{"X-Aoe-Tags":"\${AOE_HDR_X_AOE_TAGS}"}}]}
EOF
    run=("$bin" exec "hi")
    extra=()
    ;;
*) die "no probe recipe for $agent (known: ${HOST_AGENTS[*]})" ;;
esac

# The probe's own plugin config: one static tag so a delivery is unambiguous, and
# the caller can add targets (a body field, a query parameter) to test those. A
# recipe may replace the generic target set with the shape its harness can carry.
mkdir -p "$XDG_CONFIG_HOME/agent-of-empires"
{
    printf '%s\n' '[plugins."dev.oozut.llm-tags".settings]' 'tags = ["team:core"]'
    if [[ -n "$extra_settings" ]]; then cat "$extra_settings"; fi
    if [[ -n "${targets_block:-}" ]]; then printf '\n%s\n' "$targets_block"; fi
} >"$XDG_CONFIG_HOME/agent-of-empires/config.toml"


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
python3 - "$sink_log" "$EVIDENCE/$agent.json" "$agent" "$bin" "$cli" "$started_at" "$run_code" "$hook_out" "$work/run.out" <<'PY'
import json, sys, pathlib

log, out, agent, binary, cli, started_at, run_code, hook_path, run_log = sys.argv[1:10]
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


def unbase64(text):
    """`{tags_b64}`: the same JSON in a spelling a comma-splitting parser keeps."""
    import base64, binascii

    if not isinstance(text, str) or len(text) < 8:
        return None
    try:
        raw = base64.urlsafe_b64decode(text + "=" * (-len(text) % 4))
    except (binascii.Error, ValueError):
        return None
    try:
        return raw.decode("utf-8")
    except UnicodeDecodeError:
        return None


def holds(candidate):
    """The minted tag set, read out of `candidate`, or None."""
    if not candidate:
        return None
    parsed = candidate
    if isinstance(candidate, str):
        try:
            parsed = json.loads(candidate)
        except ValueError:
            decoded = unbase64(candidate)
            try:
                parsed = json.loads(decoded) if decoded else None
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


def record(where, found, name=None):
    """One delivery, saying how far it had to be decoded to be read."""
    encoding = None
    if isinstance(found, str):
        decoded = unbase64(found)
        if decoded:
            try:
                parsed = json.loads(decoded)
            except ValueError:
                parsed = None
            if isinstance(parsed, dict):
                found, encoding = parsed, "base64url"
    entry = {"where": where, "value": found}
    if name is not None:
        entry["name"] = name
    if encoding:
        entry["encoding"] = encoding
    evidence["deliveries"].append(entry)


for row in rows:
    for name, value in (row.get("headers") or {}).items():
        found = holds(str(value))
        if found is not None:
            record("header", found, name)
    found = find(row.get("body") or {})
    if found is not None:
        record("body", found)
    query = urllib.parse.parse_qs(urllib.parse.urlsplit(row.get("path", "")).query)
    for name, values in query.items():
        for value in values:
            found = holds(urllib.parse.unquote(value))
            if found is not None:
                record("query", found, name)
                break
evidence["delivered"] = bool(evidence["deliveries"])

# Why nothing arrived matters as much as that it did not: a harness that refuses
# to talk without an account is not evidence of a broken channel.
said = ""
try:
    said = pathlib.Path(run_log).read_text(errors="replace").lower()
except OSError:
    pass
if evidence["delivered"]:
    evidence["outcome"] = "delivered"
elif any(
    phrase in said
    for phrase in ("authentication failed", "please log in", "invalid api key")
):
    evidence["outcome"] = "blocked-by-auth"
else:
    evidence["outcome"] = "not-delivered"
pathlib.Path(out).write_text(json.dumps(evidence, indent=2) + "\n")
print(json.dumps(evidence, indent=2))
sys.exit(0 if evidence["delivered"] else 1)
PY
status=$?

outcome="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get("outcome",""))' \
    "$EVIDENCE/$agent.json" 2>/dev/null || true)"

if [[ $status -eq 0 ]]; then
    note "$agent: delivered (evidence docs/harness-evidence/$agent.json)"
elif [[ "$outcome" == "blocked-by-auth" ]]; then
    note "$agent: blocked-by-auth — the channel is configured but this harness refuses"
    note "to send anything without a logged-in account; recorded as evidence, not a pass."
    exit 0
else
    note "$agent: nothing delivered; hook said:"
    sed 's/^/  /' "$hook_out" >&2
    sed 's/^/  /' "$work/hook.err" >&2
    note "harness output ($work/run.out):"
    tail -5 "$work/run.out" | sed 's/^/  /' >&2
fi
exit $status
