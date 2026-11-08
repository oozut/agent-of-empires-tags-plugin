#!/bin/sh
# Check one installed copy of this plugin against the AoE host it is installed in.
#
# Every line prints PASS or FAIL with what was inspected. Exit 0 only when nothing
# failed. Nothing is written outside the temporary directory, so it is safe to run
# on a live host: it reads the install, the config and the binaries, and starts no
# session.
#
#   tools/verify-install.sh                 # the default AoE app dir and profile
#   AOE_APP_DIR=... tools/verify-install.sh # another install
#
# What it cannot check: whether a *harness* honours what is written for it. That
# needs the harness itself, and is what `tools/probe-harness.sh <agent>` and
# `tools/probe-sandbox.sh` are for.
set -u

PLUGIN=dev.oozut.llm-tags
APP_DIR=${AOE_APP_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/agent-of-empires}
INSTALLED=$APP_DIR/plugins/$PLUGIN
CLI=$INSTALLED/bin/aoe-llm-tags
CONFIG=$APP_DIR/config.toml
failed=0

pass() { printf 'PASS  %s\n' "$*"; }
fail() { printf 'FAIL  %s\n' "$*"; failed=$((failed + 1)); }
skip() { printf 'skip  %s\n' "$*"; }

# --- the install itself ------------------------------------------------------

if [ -x "$CLI" ]; then
  pass "binary $CLI"
else
  fail "no executable at $CLI (is the plugin installed in $APP_DIR?)"
  printf '\n%d check(s) failed\n' "$failed"
  exit 1
fi

if [ -x "$INSTALLED/bin/aoe-llm-tags-worker" ]; then
  pass "worker binary"
else
  fail "no worker binary: the page will render and never answer"
fi

for artefact in harness/pi.ts harness/activity.append.md harness/skills/aoe-activity/SKILL.md; do
  if [ -f "$INSTALLED/$artefact" ]; then
    pass "artefact $artefact"
  else
    fail "artefact $artefact missing: an approval would be refused with MissingArtifact"
  fi
done

if aoe plugin list 2>/dev/null | grep -q "^$PLUGIN"; then
  enabled=$(aoe plugin list 2>/dev/null | awk -v id="$PLUGIN" '$1 == id {print $4}')
  if [ "$enabled" = "enabled" ]; then
    pass "aoe plugin list: $PLUGIN enabled"
  else
    fail "aoe plugin list: $PLUGIN is '$enabled', so the worker never starts"
  fi
else
  fail "aoe plugin list does not show $PLUGIN"
fi

# The plugin must not have been granted a network capability: it never opens a
# socket, and asking for one is a second approval prompt for nothing.
if aoe plugin info "$PLUGIN" 2>/dev/null | grep -q '^  caps:.*\bnet\b'; then
  fail "the grant includes net; this plugin opens no socket"
else
  pass "capability grant has no net"
fi

# --- the hook lines ---------------------------------------------------------
#
# A pre-`--substrate` line is not legacy, it is the broken state: aoe runs
# `before_start` from inside the container paths, and without the flag the hook
# builds a copy the container cannot open.

printed=$("$CLI" setup --print --adapter pi 2>/dev/null) || printed=""
want_host=$(printf '%s\n' "$printed" | sed -n "s/^before_session = '\(.*\)'$/\1/p")
want_sandbox=$(printf '%s\n' "$printed" | sed -n "s/^before_start = '\(.*\)'$/\1/p")

if [ -z "$want_host" ] || [ -z "$want_sandbox" ]; then
  fail "setup --print did not name both keys"
else
  pass "setup --print names both keys"
fi

installed_line() {
  grep -E "^$1 = " "$CONFIG" 2>/dev/null | head -1
}

if [ -f "$CONFIG" ]; then
  pass "config $CONFIG"
  for key in before_session before_start; do
    line=$(installed_line "$key")
    if [ -z "$line" ]; then
      fail "$key is not installed in $CONFIG"
      continue
    fi
    case $key in
      before_session) want=host ;;
      *) want=sandbox ;;
    esac
    case $line in
      *"--substrate $want"*) pass "$key declares --substrate $want" ;;
      *--substrate*) fail "$key declares the other substrate; aoe passes no sandbox flag, so this is wrong for every launch" ;;
      *) fail "$key predates sandbox delivery: re-paste it from setup --print" ;;
    esac
    case $line in
      *"$INSTALLED/bin/aoe-llm-tags"*) pass "$key points at this install" ;;
      *) fail "$key points somewhere else: a moved or reinstalled plugin needs the line re-pasted" ;;
    esac
    case $line in
      *"|| true"*) pass "$key cannot abort a launch" ;;
      *) fail "$key has no '|| true': a crash would block the session" ;;
    esac
  done
else
  fail "no $CONFIG: the hooks cannot run at all"
fi

# --- what the hook actually does --------------------------------------------
#
# One host launch and one sandboxed launch, in a throwaway HOME, with the values
# aoe passes. A host launch of a pi session writes this session's copy and
# exports the override variable; a sandboxed one must write nothing under the app
# dir, because its container cannot open anything there.

tmp=$(mktemp -d) || exit 1
trap 'rm -rf "$tmp"' EXIT INT TERM
mkdir -p "$tmp/home" "$tmp/config/agent-of-empires"

row='{"id":"VERIFY","title":"Verify","tool":"pi","worktree_path":"'"$tmp"'","status":"running"}'
cat > "$tmp/bin-aoe" <<EOF
#!/bin/sh
case "\$1" in
  session) echo '$row' ;;
  list) echo '[$row]' ;;
  *) echo '{}' ;;
esac
EOF
chmod +x "$tmp/bin-aoe"
mkdir -p "$tmp/home/.pi/agent"
printf '{"providers":{"verify":{"headers":{},"models":[{"id":"m1"}]}}}\n' > "$tmp/home/.pi/agent/models.json"

# One throwaway HOME per substrate: a host launch leaves a copy behind, and the
# sandbox run has to be judged on what it wrote itself, not on what it found.
run_hook() {
  env -u https_proxy -u http_proxy -u all_proxy \
    HOME="$tmp/$1-home" XDG_CONFIG_HOME="$tmp/$1-config" AOE_BIN="$tmp/bin-aoe" \
    AOE_SESSION_ID=VERIFY AOE_TOOL=pi AOE_PROJECT_PATH="$tmp" \
    "$CLI" hook --session VERIFY --tool pi --project "$tmp" --title Verify --substrate "$1"
}

mkdir -p "$tmp/host-home/.pi/agent" "$tmp/sandbox-home/.pi/agent"
mkdir -p "$tmp/host-config/agent-of-empires" "$tmp/sandbox-config/agent-of-empires"
for home in host sandbox; do
  printf '{"providers":{"verify":{"headers":{},"models":[{"id":"m1"}]}}}\n' \
    > "$tmp/$home-home/.pi/agent/models.json"
done

host_out=$(run_hook host 2>"$tmp/host.err")
if printf '%s\n' "$host_out" | grep -q '^AOE_LLM_TAGS='; then
  pass "host launch exports AOE_LLM_TAGS"
else
  fail "host launch exported no AOE_LLM_TAGS"
fi
if printf '%s\n' "$host_out" | grep -q '^PI_CODING_AGENT_DIR='; then
  pass "host launch exports the override variable"
else
  fail "host launch wrote no session copy for pi"
fi
if [ -f "$tmp/host-config/agent-of-empires/sessions/VERIFY/pi/models.json" ]; then
  pass "host launch wrote this session's config copy"
else
  fail "host launch wrote no copy: tags stay in the environment only"
fi

sandbox_out=$(run_hook sandbox 2>"$tmp/sandbox.err")
if [ "$(printf '%s\n' "$sandbox_out" | grep -c '=')" -eq 1 ] &&
  printf '%s\n' "$sandbox_out" | grep -q '^AOE_LLM_TAGS='; then
  pass "sandboxed launch exports the variables and nothing else"
else
  fail "sandboxed launch printed more than the variables: $sandbox_out"
fi
if [ -d "$tmp/sandbox-config/agent-of-empires/sessions/VERIFY/pi" ]; then
  fail "sandboxed launch wrote a copy a container cannot read"
else
  pass "sandboxed launch wrote no copy"
fi

# --- what the doctor says ---------------------------------------------------

doctor=$("$CLI" doctor --adapter pi 2>&1)
summary=$(printf '%s\n' "$doctor" | grep -E '^[0-9]+ ok, ' | tail -1)
if printf '%s\n' "$doctor" | grep -q '\[fail\]'; then
  fail "doctor: $summary"
  printf '%s\n' "$doctor" | grep '\[fail\]' | sed 's/^/      /'
else
  pass "doctor: $summary"
fi

# --- the page and the worker ------------------------------------------------

if "$CLI" extension status --json >/dev/null 2>&1; then
  pass "extension status answers"
else
  fail "extension status failed: the page's extension panel has nothing to show"
fi

if [ -f "$APP_DIR/harness-extension.toml" ]; then
  pass "an approval exists for at least one harness"
else
  skip "no harness extension approved yet (extension enable <harness>)"
fi

printf '\n'
if [ "$failed" -eq 0 ]; then
  printf 'all checks passed\n'
  exit 0
fi
printf '%d check(s) failed\n' "$failed"
exit 1
