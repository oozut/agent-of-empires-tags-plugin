# Reference deployment (non-normative)

One concrete stack this plugin is exercised against. **Nothing here is part of the contract** —
it is a worked example, written down so the general docs never have to be read as "for LiteLLM"
or "for pi". If this deployment changes, only this file changes.

## The stack

```
aoe session
  └─ pi  ($PI_CODING_AGENT_DIR = throwaway overlay)
       └─ pi-proxy wrapper            /usr/local/bin/pi-proxy
            ├─ writes the overlay's models.json + settings.json on every launch
            ├─ exports HTTP(S)_PROXY  → OneCLI gateway URL (agent-selecting token)
            └─ upstream ONECLI_PI_UPSTREAM = https://litellm.cadot.local/v1
                 └─ LiteLLM (routes to the model provider)
```

- The plugin's own docs refer to "the consumer system". Here it is either the OneCLI proxy (it sees
  every request first) or LiteLLM (it prices them). Both are candidates for verification.
- aoe runs used to be plain `pi` only in terminal view; structured view is `pi-proxy-acp`, which
  execs `pi-proxy`. Either way the request leaves through the wrapper.

## What the wrapper forces on any header work

Verified in `/usr/local/bin/pi-proxy` (`gen_overlay`, lines 174-239):

1. **The overlay is disposable.** `PI_CODING_AGENT_DIR` is rewritten on every launch as
   `{"providers": {"litellm": …}}`. Writing a header mapping to
   `$PI_CODING_AGENT_DIR/models.json` writes to a file that the next launch deletes.
2. **Only `providers.litellm` is carried over**, copied from `$HOME/.pi/agent/models.json`
   before `baseUrl` and `apiKey` are overwritten. A header set on `providers.anthropic` is
   correct-looking and never sent.
3. **A headers-only `litellm` entry is discarded.** The copy is used only if it has a non-empty
   `models` list (`if not isinstance(lit, dict) or not lit.get("models")`), otherwise a synthetic
   provider replaces it, dropping `headers` with it. So the mapping must name at least one model.
4. **`PI_CODING_AGENT_DIR` is set in the environment and is not the source of truth.** On this
   host it is `/var/lib/aoe/.config/onecli/pi_cli/pi-agent`, which holds no `models.json`. The
   wrapper reads `$HOME/.pi/agent/models.json` (`/var/lib/aoe/.pi/agent/models.json`), which does
   not exist yet — `setup` creates it.
5. **The tag has to survive a proxy hop.** Requests go to the OneCLI URL via `HTTP(S)_PROXY`, not
   directly to LiteLLM, so a header only counts once it is observed on the far side.

## Consequences for the general docs — and the parts that stay general

| General statement | This deployment's instance |
|---|---|
| "Some harnesses launch through a wrapper that regenerates their config." | `pi-proxy` rebuilds `PI_CODING_AGENT_DIR` per launch. |
| "Write the mapping to the file the harness actually reads." | `~/.pi/agent/models.json`, provider `litellm`, with a model. |
| "Discover the config root from the harness env var, then a canonical default." | `PI_CODING_AGENT_DIR` is a trap here; the canonical default wins. |
| "Verify against the far side of every hop." | OneCLI proxy, then LiteLLM. |

The first three exist because harnesses differ. They are not why the header set is a list of
templates, why delivery is env-based, or why values are launch-scoped — those follow from the
contract alone.

## Preconditions this deployment is currently missing

- **Sandbox leg.** `pi-proxy` needs its gateway credential from a systemd credential unit
  (`/run/credentials/aoe-serve.service`) or `/etc/pi-proxy/onecli-acp.env`; neither exists inside a
  container, and `aoe` (uid 996) is not in the `docker` group, so `sandbox.enabled_by_default` is
  off and no container can be started to test against. Nothing about the contract depends on this;
  it is a host setup gap.
- **Consumer-side tagging.** Whether the OneCLI proxy forwards an unknown header untouched, and
  whether LiteLLM turns a request header into a spend dimension at all, are unknown. If it does
  not, the consumer leg needs a real change (proxy config, a metadata translation, or a different
  consumer) — and that is a deployment fact, not a reason to redesign the header set.

## Proof runs (M0-T08 mechanism leg, recorded 2026-10-29)

Two legs, never merged: this one proves the hook minted the value and the harness sent it. The
consumer leg (that the far side keeps it) is still open — see open decision #4.

| Step | Command | Result |
|---|---|---|
| Launch a session with the hook lines installed | `aoe add --scratch -t tag-sink-probe --tool pi --launch` | session `1fb406b7cbdc43c1`, title `tag-sink-probe`, tool `pi` |
| Read what the hook minted, from the launched agent's own environment | `tr '\0' '\n' < /proc/<pane-child-pid>/environ \| grep ^AOE_LLM_TAGS` | `AOE_LLM_TAGS=aoe,feature:,session:S-1fb4` |
| Start the fixture | `python3 tools/header-sink.py 8787 /tmp/tag-sink.jsonl --reset` | `header-sink listening on 127.0.0.1:8787` |
| Point a harness at the sink | `/tmp/sink-agent/models.json`: provider `sink`, `baseUrl http://127.0.0.1:8787/v1`, `headers {"X-AoE-Tags": "$AOE_LLM_TAGS"}`, one model `sink` | — |
| Send one request | `env -i HOME=/var/lib/aoe PATH=/usr/bin:/bin PI_CODING_AGENT_DIR=/tmp/sink-agent AOE_LLM_TAGS="$V" /usr/bin/pi --print --model sink/sink ping` | `ok` |
| Read the far side | `grep -o '"x-aoe-tags": "[^"]*"' /tmp/tag-sink.jsonl` | exactly one line: `"x-aoe-tags": "aoe,feature:,session:S-1fb4"` |

Notes that cost time and should not be re-derived:

- Use the **real** pi (`/usr/bin/pi`), not `/usr/local/lib/pi-proxy/bin/pi`. The shim hands off to
  `pi-proxy-acp`/`pi-proxy`, which regenerates `PI_CODING_AGENT_DIR`, so an isolated harness config
  passed in the environment is silently replaced and the sink is never reached.
- `/proc/<pid>/environ` for another user's process is unreadable; the pane shell is the aoe user's,
  so resolve `tmux list-panes -a -F "#{session_name} #{pane_pid}"`, then walk `ps --ppid <pane_pid>`
  to the agent process.
- The fixture must answer a streaming request with SSE (`data: …` + `data: [DONE]`), otherwise the
  client retries and the log gets several identical lines for one prompt.
- Pass the minted value through explicitly (`AOE_LLM_TAGS="$V"`). Running pi from this agent's own
  shell would prove pi sends a header, but not that **aoe's hook** produced the value.

## Proof run (M0-T10 second header, 2026-10-29)

Same fixture, both headers enabled in `[plugins."dev.oozut.llm-tags".settings]` (`X-AoE-Tags`,
`X-AoE-Session`), mapping lines added to both `~/.pi/agent/models.json` and the sink config.

| Step | Result |
|---|---|
| Fresh session with the two-header hook | session `9033ad4f010e44b7`; its agent environment carried `AOE_LLM_TAGS=aoe,feature:,session:S-9033` **and** `AOE_HDR_X_AOE_SESSION=9033ad4f010e44b7` |
| One request with both variables | one POST, both headers, independent values: `x-aoe-tags: aoe,feature:,session:S-9033`, `x-aoe-session: 9033ad4f010e44b7` |
| `doctor` | `10 ok, 0 failed, 1 warning` — one mapping check per enabled header, value rendering shows both `NAME=value` pairs |

Two headers cost exactly three edits and no code: one more `[[…settings.headers]]` row, one more
mapping line, one more `env` variable. That is the contract's "the set is data" claim, tested.

## Local paths worth keeping in one place

| Thing | Path / value here |
|---|---|
| aoe app dir | `/var/lib/aoe/.config/agent-of-empires` |
| Effective config | profile `profiles/main/config.toml` (absent) → global `config.toml` |
| `[host_hooks]` today | both keys set to the plugin's `hook` command (installed 2026-10-29) |
| Plugin install dir | `/var/lib/aoe/.config/agent-of-empires/plugins/dev.oozut.llm-tags/` |
| `aoe` binary | `/usr/local/bin/aoe` |
| pi agent dir | `/var/lib/aoe/.pi/agent` — `models.json` present, provider `litellm` carries `X-AoE-Tags` |
| pi-proxy shim (do not use for isolated runs) | `/usr/local/lib/pi-proxy/bin/pi` → `pi-proxy-acp` → real pi `/usr/bin/pi` |
| aoe worktree for source facts | `/var/lib/aoe/agent-of-empires-worktrees/koreans` |
