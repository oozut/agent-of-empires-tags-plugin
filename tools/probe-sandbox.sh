#!/usr/bin/env bash
# Which delivery mechanism actually survives a *sandboxed* session?
#
# A sandboxed session gets the hook's `before_start` variables as container
# environment, but the plugin's per-session config copy lives under the app dir,
# which aoe does not mount into the container.  A harness pointed there by an
# `overlay_env` (pi: `PI_CODING_AGENT_DIR`, opencode: `OPENCODE_CONFIG`, vibe:
# `VIBE_HOME`) therefore reads a path that does not exist inside the container.
#
# This probe measures candidate mechanisms inside the real sandbox image, one
# fresh container per mechanism, with a header sink *inside* the container (a
# sandbox cannot reach a sink on the host on every host).  It also creates one
# real `aoe add --sandbox` session and records what the container actually got,
# which is the half that no container-only run can prove.
#
# It also runs the plugin's *own* hook for the real session with
# `--substrate sandbox`, which is the line the fix installs: that hook must export
# the tag variables, write nothing, and need no app dir at all.
#
# Usage: tools/probe-sandbox.sh [--image IMG] [--runtime podman|docker]
#                               [--plugin CLI] [--no-session] [--no-host] [--keep]
#
#        --plugin CLI  the aoe-llm-tags to run (default: target/release/aoe-llm-tags)
#        --no-session  container legs only (no aoe, no real session)
#        --no-host     skip the host leg that shows one value serving both sides
#        --keep        keep the temporary work dir and the real session
#
# Evidence lands in docs/harness-evidence/sandbox-pi.json.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
evidence="$ROOT/docs/harness-evidence/sandbox-pi.json"
image="${AOE_SANDBOX_IMAGE:-ghcr.io/agent-of-empires/aoe-sandbox:latest}"
runtime="${AOE_SANDBOX_RUNTIME:-podman}"
aoe="${AOE_BIN_PATH:-$(command -v aoe || true)}"
plugin=""
session=1
host_leg=1
keep=""

die() { printf 'sandbox-probe: %s\n' "$*" >&2; exit 2; }
note() { printf 'sandbox-probe: %s\n' "$*" >&2; }

while (($#)); do
    case "$1" in
        --image) image="$2"; shift 2 ;;
        --runtime) runtime="$2"; shift 2 ;;
        --plugin) plugin="$2"; shift 2 ;;
        --no-session) session=0; shift ;;
        --no-host) host_leg=0; shift ;;
        --keep) keep=1; shift ;;
        -h | --help) sed -n '2,27p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) die "unknown option $1" ;;
    esac
done

[[ -n "$plugin" ]] || plugin="$ROOT/target/release/aoe-llm-tags"
[[ -x "$plugin" ]] || plugin=""
command -v "$runtime" >/dev/null || die "no $runtime"
"$runtime" image inspect "$image" >/dev/null 2>&1 || die "image $image is not local; pull it first"

work="$(mktemp -d "/tmp/aoe-sandbox-probe-XXXXXX")"
export work
mkdir -p "$work/project" "$work/overlay" "$work/store/agent" "$work/sinklog"
sink_log="$work/sinklog/sink.jsonl"
: >"$sink_log"

# The host-side sink of the host leg, and the path the sandbox copy is compared
# with: one file, so both legs write the same shape.
cat >"$work/sink.py" <<'PY'
import json, sys, http.server
log = sys.argv[1]
port = int(sys.argv[2]) if len(sys.argv) > 2 else 8899
class H(http.server.BaseHTTPRequestHandler):
    def _go(self):
        n = int(self.headers.get('content-length') or 0)
        self.rfile.read(n)
        with open(log, 'a') as f:
            f.write(json.dumps({"path": self.path, "x-aoe-tags": self.headers.get("x-aoe-tags")}) + "\n")
        body = json.dumps({"id": "x", "object": "chat.completion",
                           "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
                           "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}}).encode()
        self.send_response(200)
        self.send_header('content-type', 'application/json')
        self.send_header('content-length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    do_POST = _go
    do_GET = _go
    def log_message(self, *a):
        pass
http.server.HTTPServer(('127.0.0.1', port), H).serve_forever()
PY

session_id=""
container=""
cleanup() {
    if [[ -z "$keep" ]]; then
        if [[ -n "$session_id" && -n "$aoe" ]]; then
            "$aoe" session stop "$session_id" >/dev/null 2>&1 || true
            "$aoe" remove "$session_id" >/dev/null 2>&1 || true
        fi
        rm -rf "$work"
    else
        note "kept $work"
        [[ -n "$session_id" ]] && note "kept session $session_id"
    fi
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# The inside-the-container half of one variant.  Everything it needs arrives as
# environment; the models.json it writes points at a sink on 127.0.0.1 inside
# the container, so no host networking is involved.
# ---------------------------------------------------------------------------
cat >"$work/leg.sh" <<'LEG'
set -eu
variant="$1"
payload='{"team":"core","session":"S-sandbox"}'

guarded='"X-AoE-Tags":"!v=\"${AOE_LLM_TAGS-}\"; [ -n \"$v\" ] && printf %s \"$v\" || printf %s {}"'
literal='"X-AoE-Tags":"{\"team\":\"core\",\"session\":\"S-baked\"}"'

# where the fake "session copy" of the harness config goes, and how the harness
# is pointed at it: that pair is the whole experiment
case "$variant" in
  # The plugin's own overlay path, exactly as a sandboxed session receives it:
  # the directory is *not* created, because aoe does not mount the app dir in.
  host_overlay) dir=""; agent_dir=/var/lib/aoe/.config/agent-of-empires/sessions/probe/pi; hdr="$literal"; env_payload="" ;;
  env_only) dir=/root/.pi/agent; agent_dir="$dir"; hdr="$guarded"; env_payload="$payload" ;;
  overlay_workspace) dir=/project/.aoe-llm-tags/probe; agent_dir="$dir"; hdr="$literal"; env_payload="" ;;
  overlay_relative) dir=/project/.aoe-llm-tags/probe; agent_dir=.aoe-llm-tags/probe; hdr="$literal"; env_payload="" ;;
  overlay_mounted) dir=/plugin-sessions/probe/pi; agent_dir="$dir"; hdr="$literal"; env_payload="" ;;
  *) echo "unknown variant $variant" >&2; exit 2 ;;
esac

if [ -n "$dir" ]; then
    mkdir -p "$dir"
    cat >"$dir/models.json" <<J
{"providers":{"probe":{"baseUrl":"http://127.0.0.1:8899/v1","api":"openai-completions","apiKey":"probe","models":[{"id":"pm","name":"pm"}],"headers":{$hdr}}}}
J
fi

rm -f /work/sinklog/sink.jsonl
python3 - <<'PY' &
import json, http.server
class H(http.server.BaseHTTPRequestHandler):
    def _go(self):
        n = int(self.headers.get('content-length') or 0)
        self.rfile.read(n)
        with open('/work/sinklog/sink.jsonl', 'a') as f:
            f.write(json.dumps({"path": self.path, "x-aoe-tags": self.headers.get("x-aoe-tags")}) + "\n")
        body = json.dumps({"id": "x", "object": "chat.completion",
                           "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
                           "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}}).encode()
        self.send_response(200)
        self.send_header('content-type', 'application/json')
        self.send_header('content-length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    do_POST = _go
    do_GET = _go
    def log_message(self, *a):
        pass
http.server.HTTPServer(('127.0.0.1', 8899), H).serve_forever()
PY
sink=$!
sleep 1

cd /project 2>/dev/null || cd /
if [ -n "$env_payload" ]; then
    AOE_LLM_TAGS="$env_payload" PI_CODING_AGENT_DIR="$agent_dir" timeout 60 pi -p hi --model probe/pm >/work/pi.out 2>&1 || true
else
    PI_CODING_AGENT_DIR="$agent_dir" timeout 60 pi -p hi --model probe/pm >/work/pi.out 2>&1 || true
fi
kill "$sink" 2>/dev/null || true

req=$(wc -l < /work/sinklog/sink.jsonl 2>/dev/null || echo 0)
hdr=$(head -1 /work/sinklog/sink.jsonl 2>/dev/null || true)
printf 'RESULT\t%s\t%s\t%s\t%s\n' "$variant" "$req" "$hdr" "$(tail -1 /work/pi.out)"
LEG
chmod +x "$work/leg.sh"

# One container per variant, so nothing carries over between them.
run_variant() {
    local variant="$1" extra=()
    case "$variant" in
        overlay_workspace) extra=(-v "$work/project:/project") ;;
        overlay_mounted) extra=(-v "$work/overlay:/plugin-sessions/probe/pi") ;;
    esac
    "$runtime" run --rm -v "$work:/work" "${extra[@]}" \
        -e "HOST_HOME=$HOME" "$image" sh /work/leg.sh "$variant" 2>/dev/null \
        | grep '^RESULT' || printf 'RESULT\t%s\t0\t\tcontainer exited before a request\n' "$variant"
}

printf '\n%-18s %-8s %s\n' "mechanism" "request" "wire value / harness output"
printf '%s\n' "---------------------------------------------------------------"
results=()
for variant in host_overlay env_only overlay_workspace overlay_relative overlay_mounted; do
    line="$(run_variant "$variant")"
    results+=("$line")
    IFS=$'\t' read -r _ name req hdr pi_out <<<"$line"
    verdict=$([[ "$req" != 0 ]] && echo yes || echo "no  ")
    printf '%-18s %-8s %s\n' "$name" "$verdict" "${hdr:-$pi_out}"
done

# The host leg: the same relative value the container used, run on the host, so
# one value serving both substrates is measured rather than argued.
host_result="skipped"
if ((host_leg)) && command -v pi >/dev/null; then
    host_port="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')"
    rm -rf "$work/hostproj" && mkdir -p "$work/hostproj/.aoe-llm-tags/probe"
    sed "s#127.0.0.1:8899#127.0.0.1:$host_port#" >"$work/hostproj/.aoe-llm-tags/probe/models.json" <<'J'
{"providers":{"probe":{"baseUrl":"http://127.0.0.1:8899/v1","api":"openai-completions","apiKey":"probe","models":[{"id":"pm","name":"pm"}],"headers":{"X-AoE-Tags":"{\"team\":\"core\",\"session\":\"S-baked\"}"}}}}
J
    : >"$sink_log"
    python3 "$work/sink.py" "$sink_log" "$host_port" &
    host_sink_pid=$!
    sleep 1
    if kill -0 "$host_sink_pid" 2>/dev/null; then
        # A clean environment on purpose: this pi session's own PI_* variables
        # would otherwise point the harness at a different config dir and
        # hide the result.
        (cd "$work/hostproj" && env -i HOME="$HOME" PATH="/usr/local/bin:/usr/bin:/bin" \
            PI_CODING_AGENT_DIR=.aoe-llm-tags/probe \
            timeout 60 pi -p hi --model probe/pm >"$work/pi-host.out" 2>&1 || true)
        kill "$host_sink_pid" 2>/dev/null || true
        host_lines=$(wc -l <"$sink_log" 2>/dev/null || echo 0)
        host_result=$([[ "$host_lines" != 0 ]] && echo "delivered with the same relative value" || echo "no request on the host")
    else
        host_result="sink did not start (port 8899 busy?)"
    fi
    printf '\nhost leg (relative overlay dir, same value as the container): %s\n' "$host_result"
fi

# ---------------------------------------------------------------------------
# The real-session leg: what an `aoe add --sandbox` session actually receives.
# ---------------------------------------------------------------------------
session_json="null"
if ((session)); then
    [[ -n "$aoe" && -x "$aoe" ]] || die "no aoe binary; pass AOE_BIN_PATH or --no-session"
    title="sandbox-probe-$(date +%H%M%S)"
    created="$("$aoe" add --sandbox --scratch -t "$title" -c 'pi --version' -l 2>&1 || true)"
    session_id="$(printf '%s\n' "$created" | sed -n 's/^ *ID: *//p' | head -1)"
    [[ -n "$session_id" ]] || {
        printf '%s\n' "$created" >&2
        die "could not create a sandboxed session"
    }
    for _ in $(seq 30); do
        container="$("$runtime" ps --format '{{.Names}}' | grep -m1 "aoe-sandbox-${session_id:0:8}" || true)"
        [[ -n "$container" ]] && break
        sleep 1
    done
    [[ -n "$container" ]] || die "no container for $session_id"
    env_inside="$("$runtime" exec "$container" sh -c 'env | sort' 2>/dev/null || true)"
    agent_dir="$(printf '%s\n' "$env_inside" | sed -n 's/^PI_CODING_AGENT_DIR=//p' | head -1)"
    dir_inside="absent"
    if [[ -n "$agent_dir" ]]; then
        if "$runtime" exec "$container" test -d "$agent_dir" 2>/dev/null; then
            dir_inside="present"
        fi
    fi
    models="$("$runtime" exec "$container" sh -c 'test -f /root/.pi/agent/models.json && grep -c "X-AoE-Tags" /root/.pi/agent/models.json' 2>/dev/null || echo 0)"
    # The staged store names its credential by variable; without a value for it
    # the harness would report no models for both listings, which would hide the
    # difference the overlay makes. A dummy value is enough to enumerate.
    store_json="$("$runtime" exec "$container" cat /root/.pi/agent/models.json 2>/dev/null || true)"
    creds="$(printf '%s' "$store_json" | python3 -c 'import re,sys; print(" ".join(n + "=probe" for n in sorted(set(re.findall(r"\$([A-Za-z_][A-Za-z_0-9]*)", sys.stdin.read())))))' 2>/dev/null || true)"
    # The exact guard `setup --apply` ships in the user's own models.json, run
    # inside the real container: no overlay, no plugin file, just the variable.
    guard="$("$runtime" exec "$container" sh -c 'v="${AOE_LLM_TAGS-}"; [ -n "$v" ] && printf %s "$v" || printf %s {}' 2>/dev/null || true)"
    # The plugin's own sandbox line, run as aoe runs it: `$SHELL -c`, the same
    # AOE_* variables, and — deliberately — no HOME, because a sandboxed hook
    # must not need the app dir it is not allowed to rely on.
    plugin_out=""
    plugin_err=""
    plugin_code=""
    if [[ -n "$plugin" ]]; then
        plugin_err_file="$work/plugin-stderr"
        plugin_code=0
        plugin_out="$(env -i PATH="/usr/local/bin:/usr/bin:/bin" AOE_BIN="${aoe:-aoe}" \
            AOE_SESSION_ID="$session_id" AOE_TOOL=pi HOME= XDG_CONFIG_HOME= \
            "$plugin" hook --session "$session_id" --tool pi --substrate sandbox \
            2>"$plugin_err_file")" || plugin_code=$?
        plugin_err="$(cat "$plugin_err_file")"
    fi
    listed="$("$runtime" exec "$container" env -u PI_CODING_AGENT_DIR sh -c "$creds pi --list-models" 2>&1 | head -2 || true)"
    broken="$("$runtime" exec "$container" sh -c "$creds pi --list-models" 2>&1 | head -2 || true)"
    session_json="$(SID="$session_id" CNAME="$container" AENV="$env_inside" AGENT_DIR="$agent_dir" DIR_INSIDE="$dir_inside" MODELS="$models" LISTED="$listed" GUARD="$guard" BROKEN="$broken" PLUGIN="$plugin" POUT="$plugin_out" PERR="$plugin_err" PCODE="$plugin_code" python3 - <<'PY'
import json, os
def lines(name):
    return [line for line in os.environ.get(name, "").splitlines() if line.strip()]
container_tags = [l.split("=", 1)[1] for l in lines("AENV") if l.startswith("AOE_LLM_TAGS=")]
hook_lines = lines("POUT")
print(json.dumps({
    "session_id": os.environ["SID"],
    "container": os.environ["CNAME"],
    "hook_variables": [l for l in lines("AENV") if l.startswith(("AOE_", "PI_CODING_AGENT_DIR="))],
    "overlay_env": {"name": "PI_CODING_AGENT_DIR", "value": os.environ["AGENT_DIR"], "inside_container": os.environ["DIR_INSIDE"]},
    "staged_store_has_mapping": os.environ["MODELS"] != "0",
    "models_without_overlay": lines("LISTED")[:2],
    "models_with_overlay": lines("BROKEN")[:2],
    "staged_guarded_mapping_resolves_to": os.environ.get("GUARD", ""),
    "created_with": "the before_start line installed on this host at probe time",
    "plugin_sandbox_hook": {
        "cli": os.environ.get("PLUGIN", ""),
        "argv": ["hook", "--substrate", "sandbox"],
        "exit_code": os.environ.get("PCODE", ""),
        "environment": "AOE_* only: no HOME, no XDG_CONFIG_HOME",
        "lines": hook_lines,
        "writes_an_overlay_env_line": any(
            not line.startswith("AOE_") for line in hook_lines),
        "stderr": lines("PERR"),
        "same_payload_as_the_container": bool(container_tags) and hook_lines == [
            "AOE_LLM_TAGS=" + container_tags[0]],
    },
}))
PY
)"
    printf '\nreal sandboxed session %s (container %s):\n' "$session_id" "$container"
    printf '  hook variables inside the container: %s\n' \
        "$(printf '%s\n' "$env_inside" | grep -cE '^AOE_') AOE_* variable(s)"
    printf '  PI_CODING_AGENT_DIR=%s  (%s inside the container)\n' "${agent_dir:-unset}" "$dir_inside"
    printf '  staged store mapping: %s\n' "$([[ "$models" != 0 ]] && echo present || echo 'absent')"
    printf '  pi --list-models with it:     %s\n' "${broken%%$'\n'*}"
    printf '  pi --list-models without it:  %s\n' "${listed%%$'\n'*}"
    if [[ -n "$plugin" ]]; then
        printf '  plugin hook --substrate sandbox: %s line(s), overlay env: %s\n' \
            "$(printf '%s\n' "$plugin_out" | grep -c '=' || true)" \
            "$(printf '%s\n' "$plugin_out" | grep -qv '^AOE_' && echo yes || echo no)"
    fi
fi

python3 - "$evidence" "$image" "$host_result" "$session_json" "${results[@]}" <<'PY'
import json, sys
out, image, host_result, session_json, rows = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5:]
variants = []
for row in rows:
    parts = row.split("\t")
    if len(parts) < 5:
        continue
    _, name, count, header, pi_out = parts[:5]
    delivered = None
    if header:
        try:
            delivered = json.loads(header).get("x-aoe-tags")
        except Exception:
            delivered = header
    variants.append({
        "mechanism": name,
        "requests": int(count or 0),
        "delivered_value": delivered,
        "harness_output": pi_out,
        "sandbox_verdict": "delivered" if int(count or 0) else "no request",
    })
evidence = {
    "image": image,
    "what_this_proves": "a sandboxed session receives the hook's environment variables, and the "
                        "static mapping in the store aoe seeds reads one, so the tag travels "
                        "without a config copy; the copy the plugin writes is under the app dir, "
                        "which aoe does not mount, so an overlay_env pointing there is a path the "
                        "harness cannot read — `real_session` shows that on a host whose "
                        "before_start line predates `--substrate`, and "
                        "`real_session.plugin_sandbox_hook` shows the shipped sandbox line "
                        "exporting the same payload with no overlay env and no app dir at all",
    "variants": variants,
    "host_leg": host_result,
    "real_session": json.loads(session_json),
}
with open(out, "w") as f:
    f.write(json.dumps(evidence, indent=2) + "\n")
print(json.dumps({"variants": [(v["mechanism"], v["sandbox_verdict"]) for v in variants],
                  "real_session": evidence["real_session"] and evidence["real_session"].get("session_id")}, indent=2))
PY
note "evidence: $evidence"
