#!/usr/bin/env bash
# One *real* Agent of Empires session per harness, proving every delivery kind a
# harness can carry: aoe creates the session, aoe runs the hook, the hook builds
# the session's own config, and the harness sends the request to a local sink.
#
# Nothing outside this run's temporary HOME is written: aoe, the plugin and the
# harness all see a fresh $HOME/$XDG_CONFIG_HOME, so this is repeatable on any
# machine and safe to run against a machine with real sessions.
#
# Usage: tools/probe-session.sh <agent> [--aoe PATH] [--cli PATH] [--bin PATH]
#                                [--sink PORT] [--timeout SECONDS] [--keep]
#        tools/probe-session.sh --list
#
# Evidence lands in docs/harness-evidence/session-<agent>.json: the session aoe
# created, the config the hook built for it, and what arrived at the sink.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
AGENTS=(pi claude codex opencode)

die() { printf 'session-probe: %s\n' "$*" >&2; exit 2; }
note() { printf 'session-probe: %s\n' "$*" >&2; }

usage() {
    sed -n '2,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

agent=""
aoe="${AOE_BIN_PATH:-}"
cli="${AOE_LLM_TAGS_CLI:-}"
bin=""
sink_port="${AOE_PROBE_PORT:-8899}"
wait_seconds=120
keep=""
while (($#)); do
    case "$1" in
        --list) printf '%s\n' "${AGENTS[@]}"; exit 0 ;;
        --aoe) aoe="$2"; shift 2 ;;
        --cli) cli="$2"; shift 2 ;;
        --bin) bin="$2"; shift 2 ;;
        --sink) sink_port="$2"; shift 2 ;;
        --timeout) wait_seconds="$2"; shift 2 ;;
        --keep) keep=1; shift ;;
        -h | --help) usage 0 ;;
        -*) die "unknown option $1" ;;
        *) agent="$1"; shift ;;
    esac
done
[[ -n "$agent" ]] || usage 2

aoe="${aoe:-$(command -v aoe || true)}"
[[ -n "$aoe" && -x "$aoe" ]] || die "no aoe binary; pass --aoe <path>"

# The plugin CLI the host hooks run: the installed one, else this tree's build.
if [[ -z "$cli" ]]; then
    for candidate in \
        "$HOME/.config/agent-of-empires/plugins/dev.oozut.llm-tags/bin/aoe-llm-tags" \
        "$ROOT/target/debug/aoe-llm-tags" \
        "$ROOT/target/release/aoe-llm-tags"; do
        [[ -x "$candidate" ]] && cli="$candidate" && break
    done
fi
[[ -n "$cli" && -x "$cli" ]] || die "no aoe-llm-tags binary; pass --cli <path>"

if [[ -z "$bin" ]]; then
    for candidate in "$HOME/.local/bin/$agent" "/usr/local/bin/$agent" "/usr/bin/$agent"; do
        [[ -x "$candidate" ]] && bin="$candidate" && break
    done
    bin="${bin:-$(command -v "$agent" 2>/dev/null || true)}"
fi
[[ -n "$bin" && -x "$bin" ]] || die "$agent is not installed; pass --bin <path>"

work="$(mktemp -d "/tmp/aoe-session-$agent-XXXXXX")"
export HOME="$work/home"
export XDG_CONFIG_HOME="$work/home/.config"
mkdir -p "$HOME" "$XDG_CONFIG_HOME" "$work/project"

# aoe finds a harness under `$HOME/.local/bin` too, and this run's HOME is fresh:
# point that at the real binary so `aoe add` sees the agent as installed.
mkdir -p "$HOME/.local/bin"
ln -sf "$bin" "$HOME/.local/bin/$agent"
export PATH="$HOME/.local/bin:$PATH"
project="$work/project"

sink_log="$work/sink.jsonl"
python3 "$ROOT/tools/header-sink.py" "$sink_port" "$sink_log" --reset >"$work/sink.out" 2>&1 &
sink_pid=$!
session_id=""
cleanup() {
    kill "$sink_pid" 2>/dev/null || true
    [[ -n "$session_id" ]] && "$aoe" session stop "$session_id" >/dev/null 2>&1 || true
}
keepwork() {
    if [[ -n "$keep" ]]; then note "kept $work"; else rm -rf "$work"; fi
}
trap 'cleanup; keepwork' EXIT
for _ in $(seq 50); do
    grep -q listening "$work/sink.out" 2>/dev/null && break
    sleep 0.1
done

base="http://127.0.0.1:$sink_port"

# The harness config a user would already have, pointed at the sink instead of a
# real endpoint. This is the *source* the hook copies from, never edited by us.
# One-shot invocation per harness: a real session, but a prompt it can answer
# without a terminal, so the request actually leaves the process.
case "$agent" in
pi)
    cmd="$bin -p hi"
    mkdir -p "$HOME/.pi/agent"
    cat >"$HOME/.pi/agent/models.json" <<EOF
{"providers":{"probe":{"baseUrl":"$base/v1","api":"openai-completions","apiKey":"probe","models":[{"id":"probe-model","name":"probe-model"}]}}}
EOF
    ;;
claude)
    cmd="$bin -p hi"
    # aoe launches the pane with HOME set from the passwd entry, not ours, and
    # drops ANTHROPIC_* from the environment, so claude is pointed at this
    # fixture the way the plugin points a harness at anything: an env var the
    # hook exports. The endpoint itself lives in the fixture's settings.json.
    mkdir -p "$HOME/.claude"
    # aoe drops ANTHROPIC_* from the launch environment, so the endpoint and the
    # fixture credentials live in the harness's own settings, as a user's would.
    cat >"$HOME/.claude/settings.json" <<EOF
{"env":{"ANTHROPIC_BASE_URL":"$base","ANTHROPIC_AUTH_TOKEN":"probe","ANTHROPIC_MODEL":"probe-model","ANTHROPIC_SMALL_FAST_MODEL":"probe-model","CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC":"1"}}
EOF
    printf '{"probe":{"accessToken":"probe"}}\n' >"$HOME/.claude/.credentials.json"
    ;;
codex)
    cmd="$bin exec --skip-git-repo-check hi"
    mkdir -p "$HOME/.codex"
    cat >"$HOME/.codex/config.toml" <<EOF
model = "probe-model"
model_provider = "probe"

[model_providers.probe]
name = "probe"
base_url = "$base/v1"
env_key = "PROBE_KEY"
wire_api = "responses"
EOF
    ;;
opencode)
    cmd="$bin run --model probe/probe-model hi"
    mkdir -p "$XDG_CONFIG_HOME/opencode"
    cat >"$XDG_CONFIG_HOME/opencode/opencode.json" <<EOF
{"provider":{"probe":{"npm":"@ai-sdk/openai-compatible","name":"probe","options":{"baseURL":"$base/v1","apiKey":"probe"},"models":{"probe-model":{"name":"probe-model"}}}}}
EOF
    ;;
*) die "no session recipe for $agent (known: ${AGENTS[*]})" ;;
esac

# The host's config: the two hook lines a user pastes once, and one target of
# every kind this plugin knows. A harness that cannot carry a kind must refuse
# it in `doctor` and skip it, never write a lie.
mkdir -p "$XDG_CONFIG_HOME/agent-of-empires"
hooks="$(env AOE_BIN="$aoe" "$cli" setup --print --adapter "$agent" 2>/dev/null |
    grep -E '^(before_session|before_start) = ' || true)"
[[ -n "$hooks" ]] || die "$cli setup --print produced no hook lines"
{
    printf '[host_hooks]\n%s\n\n' "$hooks"
    # One target per kind, all on the default format: each carries the tag set as
    # one JSON object, so the sink can prove a field arrived, not a string.
    printf '%s\n' '[plugins."dev.oozut.llm-tags".settings]' \
        'tags = ["team:core", "feature:billing", "cost:=1.5"]'
    printf '%s\n' \
        '' '[[plugins."dev.oozut.llm-tags".settings.targets]]' 'kind = "header"' \
        'name = "X-AoE-Tags"' \
        '' '[[plugins."dev.oozut.llm-tags".settings.targets]]' 'kind = "query"' \
        'name = "aoe_tags"' \
        '' '[[plugins."dev.oozut.llm-tags".settings.targets]]' 'kind = "body"' \
        'name = "metadata"' \
        '' '[[plugins."dev.oozut.llm-tags".settings.targets]]' 'kind = "header"' \
        'name = "X-AoE-Flat"' 'format = "csv"' 'template = "aoe,{tags},session:{session}"'
} >"$XDG_CONFIG_HOME/agent-of-empires/config.toml"

# Env the launched harness needs that is not part of the tag contract.
case "$agent" in
claude) : ;;
codex) export PROBE_KEY=probe OPENAI_API_KEY=probe ;;
esac

# aoe refuses to launch a host session until it may write its own agent hooks;
# this run's HOME has no approval on record yet.
if [[ -z "${AOE_PROBE_NO_APPROVE:-}" ]]; then
    "$aoe" hooks approve >"$work/approve.out" 2>&1 ||
        note "aoe hooks approve failed (continuing)"
fi

# aoe launches every agent with HOME set from the passwd entry and drops most of
# the environment, so a harness whose config lives in `$HOME` cannot be pointed
# at the sink from outside. Record what the pane actually received instead: that
# is the channel the plugin uses (the value rides the launch environment).
cmd="sh -c 'env > \"$work/pane-env.txt\"; exec $cmd'"

# 1. aoe creates the session.
# `--cmd` infers the tool from the binary name, which is what makes the session
# the same shape a real one has (`tool`, command, hook env) without a terminal.
"$aoe" add --scratch --cmd "$cmd" -t "probe-$agent" >"$work/add.out" 2>&1 ||
    { cat "$work/add.out" >&2; die "aoe add failed"; }
session_id="$("$aoe" list --json 2>/dev/null | python3 -c '
import json, sys
rows = json.load(sys.stdin)
rows = rows.get("sessions", rows) if isinstance(rows, dict) else rows
row = next(row for row in rows if row.get("title") == sys.argv[1])
if row.get("tool") != sys.argv[2]:
    sys.exit("aoe inferred tool %r, expected %r" % (row.get("tool"), sys.argv[2]))
print(row["id"])
' "probe-$agent" "$agent")" || die "aoe did not create the session (tool inference?)"

# 2. aoe starts it: the hook runs, builds the session copy, and the harness runs.
"$aoe" session start "$session_id" >"$work/start.out" 2>&1 ||
    note "aoe session start reported an error; the hook may still have run"

# 3. Wait for the sink, then let the session settle.
for _ in $(seq "$((wait_seconds * 4))"); do
    [[ -s "$sink_log" ]] && sleep 1 && break
    sleep 0.25
done
sleep 1

# What the agent printed is the only clue when a launch goes wrong, so keep it
# before the session is stopped.
"$aoe" session capture "$session_id" >"$work/pane.out" 2>&1 || true
overlay="$XDG_CONFIG_HOME/agent-of-empires/sessions/$session_id/$agent"
"$aoe" session stop "$session_id" >/dev/null 2>&1 || true

evidence_dir="$ROOT/docs/harness-evidence"
mkdir -p "$evidence_dir"
python3 - "$sink_log" "$evidence_dir/session-$agent.json" "$agent" "$bin" "$cli" \
    "$session_id" "$overlay" "$base" "$work" <<'PY'
import json, os, sys, pathlib, urllib.parse

log, out, agent, binary, cli, session, overlay, base, work = sys.argv[1:10]
rows = []
if pathlib.Path(log).exists():
    rows = [json.loads(line) for line in open(log) if line.strip()]

# What the hook actually built for this session: our file, and what else is there.
overlay_path = pathlib.Path(overlay)
files = {}
if overlay_path.exists():
    for entry in sorted(overlay_path.iterdir()):
        files[entry.name] = "symlink" if entry.is_symlink() else "file"

pane_env = {}
env_path = pathlib.Path(work, "pane-env.txt")
if env_path.exists():
    for line in env_path.read_text(errors="replace").splitlines():
        name, _, value = line.partition("=")
        if not name or not value:
            continue
        # The value may be the object itself (AOE_*) or wrap it
        # (`ANTHROPIC_CUSTOM_HEADERS="X-AoE-Tags: {...}"`), so try both.
        candidates = [value]
        if "{" in value and "}" in value:
            candidates.append(value[value.index("{") : value.rindex("}") + 1])
        for candidate in candidates:
            try:
                parsed = json.loads(candidate)
            except ValueError:
                continue
            if isinstance(parsed, dict) and parsed.get("team") == "core":
                pane_env[name] = parsed
                break

built = ""
for name in ("models.json", "config.toml", "opencode.json"):
    candidate = overlay_path / name
    if candidate.is_file() and not candidate.is_symlink():
        built = candidate.read_text()
        break

# A delivery counts when a *field* survives the trip: the payload is one JSON
# object, so `team` must read `core` on the far side, not merely appear in a
# string. `csv` targets are checked against the flat form they advertise.
def payload_of(value):
    """The tag object a harness carried, or None."""
    if isinstance(value, str):
        try:
            value = json.loads(value)
        except ValueError:
            return None
    if isinstance(value, dict) and value.get("team") == "core":
        return value
    return None


def find_payload(value):
    """The first tag object inside a JSON body, at any depth."""
    found = payload_of(value)
    if found:
        return found
    if isinstance(value, dict):
        for item in value.values():
            found = find_payload(item)
            if found:
                return found
    if isinstance(value, list):
        for item in value:
            found = find_payload(item)
            if found:
                return found
    return None


deliveries = []
for row in rows:
    for name, value in (row.get("headers") or {}).items():
        found = payload_of(value)
        if found:
            deliveries.append({"where": "header", "name": name, "payload": found})
        elif "aoe" in str(value) and "team:core" in str(value):
            deliveries.append({"where": "header", "name": name, "value": value})
    found = find_payload(row.get("body") or {})
    if found:
        deliveries.append({"where": "body", "payload": found})
    path = row.get("path", "")
    query = urllib.parse.parse_qs(urllib.parse.urlsplit(path).query)
    for name, values in query.items():
        for value in values:
            found = payload_of(urllib.parse.unquote(value))
            if found:
                deliveries.append({"where": "query", "name": name, "payload": found})
                break

kinds = {item["where"] for item in deliveries}
first = next((item["payload"] for item in deliveries if "payload" in item), {})
start = pathlib.Path(work, "start.out").read_text() if pathlib.Path(work, "start.out").exists() else ""
pane = pathlib.Path(work, "pane.out").read_text() if pathlib.Path(work, "pane.out").exists() else ""
if deliveries:
    outcome = "delivered"
elif "changed the agent hook path" in start or "have not been approved" in start:
    # aoe pins an agent's hook file for the launch, so a session that would move
    # it is refused before the harness starts. Nothing was lied about: no value
    # was written, and the reference channel is how this agent gets its header.
    outcome = "refused-by-aoe"
elif rows:
    outcome = "request-without-the-tag"
else:
    outcome = "no-request"
evidence = {
    "agent": agent,
    "binary": binary,
    "cli": cli,
    "session_id": session,
    "overlay": overlay,
    "overlay_entries": files,
    "overlay_carries": {
        "header": "x-aoe-tags" in built.lower(),
        "query": "aoe_tags" in built,
        "body": "samplingParams" in built or "queryParams" in built,
    },
    "requests": len(rows),
    "deliveries": deliveries,
    "delivered": sorted(kinds),
    # What the launched harness could read from its own environment. For an agent
    # whose config file aoe owns, this is the delivery channel a reference uses.
    "env_payloads": dict(sorted(pane_env.items())),
    "outcome": outcome,
    "aoe_message": next(
        (line for line in start.splitlines() if "Error:" in line), ""
    ) or next((line for line in pane.splitlines() if line.strip()), ""),
    # What the far side could read as a field, rather than as a string.
    "payload_fields": dict(sorted(first.items())),
    "run_dir": work,
}
pathlib.Path(out).write_text(json.dumps(evidence, indent=2) + "\n")
print(json.dumps({k: evidence[k] for k in
                  ("agent", "session_id", "overlay_entries", "requests", "delivered",
                   "env_payloads", "outcome")}, indent=2))
if not deliveries:
    sys.exit(1)
PY
status=$?
if [[ $status -ne 0 ]]; then
    note "$agent: nothing delivered; hook config was:"
    sed -n '1,40p' "$XDG_CONFIG_HOME/agent-of-empires/config.toml" >&2
    note "harness output:"
    tail -25 "$work/pane.out" >&2 || true
    exit 1
fi
note "$agent: delivered (evidence docs/harness-evidence/session-$agent.json)"
