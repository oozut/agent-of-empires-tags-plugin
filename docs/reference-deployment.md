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

## Local paths worth keeping in one place

| Thing | Path / value here |
|---|---|
| aoe app dir | `/var/lib/aoe/.config/agent-of-empires` |
| Effective config | profile `profiles/main/config.toml` (absent) → global `config.toml` |
| `[host_hooks]` today | empty |
| Plugin install dir | `…/plugins/dev.oozut.llm-tags/` (not created yet) |
| `aoe` binary | `/usr/local/bin/aoe` |
| pi agent dir | `/var/lib/aoe/.pi/agent` (no `models.json` yet) |
| aoe worktree for source facts | `/var/lib/aoe/agent-of-empires-worktrees/koreans` |
