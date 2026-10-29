"""``doctor`` (M0-T11): one line per check, exit 1 if any of them fails.

The checks are the six ways an install silently sends nothing
(``plan/m0.md`` acceptance 6): a missing hook key, a hook line pointing at a
plugin that moved, an unresolvable ``aoe``, an unmapped header, a template that
cannot render, and a session row the hook cannot read.

Every line prints the absolute path it looked at — a hook debugging session with
no visible paths is the expensive failure mode. ``--json`` emits the same checks
for a script or a later pane.
"""

from __future__ import annotations

import json
import os
import shlex
from dataclasses import dataclass, field
from pathlib import Path
from typing import Mapping, Sequence

from . import adapters as adapters_module
from . import resolver, settings, setup

OK = "ok"
FAIL = "fail"
WARN = "warn"
SKIP = "skip"

HOOK_CHECKS = (
    ("before_session", "host sessions will not send headers"),
    ("before_start", "sandboxed sessions will not send headers"),
)


@dataclass(frozen=True)
class Check:
    name: str
    status: str
    detail: str


@dataclass(frozen=True)
class Report:
    checks: tuple[Check, ...]
    paths: dict[str, str] = field(default_factory=dict)
    profile: str = ""
    adapter: str = ""

    @property
    def ok(self) -> bool:
        return not any(check.status == FAIL for check in self.checks)

    def counts(self) -> dict[str, int]:
        counts = {OK: 0, FAIL: 0, WARN: 0, SKIP: 0}
        for check in self.checks:
            counts[check.status] = counts.get(check.status, 0) + 1
        return counts


def run(
    flags: resolver.Flags | None = None,
    *,
    env: Mapping[str, str] | None = None,
    root: Path | None = None,
) -> Report:
    """Run every check. Never raises for a broken install; that is a failed line."""
    env = os.environ if env is None else env
    flags = flags or resolver.Flags()
    checks: list[Check] = []

    app_dir = settings.app_dir(env)
    config = settings.load_config(flags.profile, root=app_dir, env=env)
    global_path = settings.global_config_path(app_dir)
    profile_path = settings.profile_config_path(app_dir, flags.profile)
    cli = setup.cli_path(root)
    aoe = resolver.aoe_bin(env)

    checks.append(_check_effective_config(config, global_path, profile_path))

    hooks: dict[str, str] = {}
    for key, message in HOOK_CHECKS:
        entries = settings.host_hook_entries(config.table, key)
        ours = next((entry for entry in entries if "aoe-llm-tags" in entry), "")
        if ours:
            hooks[key] = ours
            checks.append(Check(f"{key} hook", OK, _abbreviate(ours)))
        elif entries:
            checks.append(
                Check(
                    f"{key} hook",
                    FAIL,
                    f"{len(entries)} hook(s) in {_files(config)} but none runs aoe-llm-tags; "
                    f"{message}",
                )
            )
        else:
            checks.append(
                Check(
                    f"{key} hook",
                    FAIL,
                    f"no [host_hooks] {key} in {_files(config)}; {message} "
                    f"(add the line from `aoe-llm-tags setup --print`)",
                )
            )

    checks.append(_check_command_path(hooks, cli))
    checks.append(_check_aoe(aoe, env))

    adapters, adapter_warnings = adapters_module.load(root)
    snap = resolver.snapshot(flags, env=env)
    adapter = _resolve_adapter(flags, snap.facts.tool, adapters)
    checks.append(_check_adapter(adapter, snap.facts.tool, adapters, adapter_warnings))
    checks.append(_check_session(snap, flags))

    harness: Path | None = None
    if adapter is not None:
        harness, _ = adapters_module.config_path(adapter, env)
    for item in snap.resolved:
        checks.append(_check_mapping(adapter, item, env))
    checks.append(_check_values(snap))

    last_verified = snap.raw.get("last_verified")
    if isinstance(last_verified, str) and last_verified.strip():
        checks.append(Check("last verification", OK, last_verified.strip()))
    else:
        checks.append(
            Check(
                "last verification",
                WARN,
                "never verified against a consumer system; proving arrival is M0-T09",
            )
        )

    paths = {
        "app dir": str(app_dir),
        "config": str(global_path),
        "profile config": str(profile_path) if profile_path else "",
        "plugin CLI": str(cli),
        "aoe binary": aoe,
        "harness config": str(harness) if harness else "",
    }
    return Report(
        checks=tuple(checks),
        paths=paths,
        profile=flags.profile,
        adapter=adapter.agent if adapter else "",
    )


def format_text(report: Report) -> str:
    lines = [f"aoe-llm-tags doctor — profile {report.profile or '(global)'}"]
    lines.append("paths:")
    for label, value in report.paths.items():
        lines.append(f"  {label:<15} {value or '(none)'}")
    lines.append("checks:")
    width = max(len(check.name) for check in report.checks)
    for check in report.checks:
        lines.append(f"  [{check.status:<4}] {check.name:<{width}}  {check.detail}")
    counts = report.counts()
    lines.append(
        f"{counts[OK]} ok, {counts[FAIL]} failed, {counts[WARN]} warning(s), "
        f"{counts[SKIP]} skipped"
    )
    return "\n".join(lines)


def format_json(report: Report) -> str:
    return json.dumps(
        {
            "ok": report.ok,
            "profile": report.profile,
            "adapter": report.adapter,
            "paths": report.paths,
            "checks": [
                {"check": check.name, "status": check.status, "detail": check.detail}
                for check in report.checks
            ],
        },
        indent=2,
    )


# --- individual checks ------------------------------------------------------


def _check_effective_config(
    config: settings.RawConfig, global_path: Path, profile_path: Path | None
) -> Check:
    if not config.present:
        return Check(
            "effective config",
            FAIL,
            f"no config file at {global_path}"
            + (f" or {profile_path}" if profile_path else "")
            + "; nothing can be wired",
        )
    if not config.sources:
        return Check(
            "effective config",
            FAIL,
            f"{', '.join(config.present)} could not be parsed: {'; '.join(config.warnings)}",
        )
    return Check("effective config", OK, "read " + ", ".join(config.sources))


def _check_command_path(hooks: Mapping[str, str], cli: Path) -> Check:
    entry = hooks.get("before_session") or hooks.get("before_start") or ""
    path = Path(_command_path(entry)) if entry else cli
    if not path.is_file() or not os.access(path, os.X_OK):
        return Check(
            "command path",
            FAIL,
            f"{path} is missing or not executable: the plugin moved, was uninstalled, "
            "or its id changed (run `aoe plugin update dev.oozut.llm-tags`)",
        )
    return Check("command path", OK, f"{path} (executable)")


def _check_aoe(aoe: str, env: Mapping[str, str]) -> Check:
    source = "from AOE_BIN" if env.get("AOE_BIN") else "from PATH" if aoe != resolver.AOE_FALLBACK else "fallback"
    if not os.access(aoe, os.X_OK):
        return Check(
            "aoe binary",
            FAIL,
            f"{aoe} ({source}) is not executable; set AOE_BIN to this host's aoe binary",
        )
    return Check("aoe binary", OK, f"{aoe} ({source})")


def _check_adapter(
    adapter: adapters_module.Adapter | None,
    tool: str,
    adapters: Sequence[adapters_module.Adapter],
    warnings: Sequence[str],
) -> Check:
    if adapter is None:
        known = ", ".join(sorted(row.agent for row in adapters)) or "none"
        if tool in ("", resolver.DEFAULT_TOOL):
            return Check(
                "harness adapter",
                WARN,
                f"no tool given (not in a session shell); pass --adapter to check a mapping "
                f"(known: {known})",
            )
        return Check(
            "harness adapter",
            FAIL,
            f"tool {tool!r} has no adapter row (known: {known}); pass --adapter",
        )
    if adapter.mechanism == "unknown" or not adapter.mapping_format or adapter.status == "needs_research":
        return Check(
            "harness adapter",
            FAIL,
            f"adapter {adapter.agent} is {adapter.status} with mechanism {adapter.mechanism!r}; "
            "the header cannot be mapped yet",
        )
    detail = f"{adapter.agent} ({adapter.mechanism}, {adapter.status})"
    if warnings:
        detail += f"; {'; '.join(warnings)}"
    return Check("harness adapter", OK, detail)


def _check_session(snap: resolver.Snapshot, flags: resolver.Flags) -> Check:
    if not flags.session:
        return Check(
            "session row",
            SKIP,
            "no --session given; the row lookup was not exercised "
            "(hook environment is the fallback)",
        )
    fallback = [warning for warning in snap.warnings if "falling back to the hook environment" in warning]
    if fallback:
        return Check(
            "session row",
            FAIL,
            f"{fallback[0]} (id {snap.facts.id or '(none)'})",
        )
    facts = snap.facts
    return Check(
        "session row",
        OK,
        f"id {facts.id} title {facts.title!r} path {facts.path or '(none)'} "
        f"group {facts.group or '(none)'} tool {facts.tool}",
    )


def _check_mapping(
    adapter: adapters_module.Adapter | None,
    item: resolver.ResolvedHeader,
    env: Mapping[str, str],
) -> Check:
    name = f"mapping {item.header}"
    if adapter is None:
        return Check(name, SKIP, "no adapter row; nothing to look in")

    if adapter.mechanism == "custom_headers_env":
        return _check_env_mapping(adapter, item, env)

    path, warnings = adapters_module.config_path(adapter, env)
    if path is None:
        return Check(name, FAIL, "; ".join(warnings) or f"adapter {adapter.agent} has no config path")
    if not path.is_file():
        return Check(
            name,
            FAIL,
            f"{path} does not exist; the header is not mapped "
            "(add the line from `aoe-llm-tags setup --print`)",
        )
    try:
        found, notes, known = _scan_file(adapter, path, item.header)
    except Exception as exc:  # a config we cannot parse is a failed check, not a crash
        return Check(name, FAIL, f"{path} could not be read: {exc}")
    if not found:
        return Check(
            name,
            FAIL,
            f"{item.header} is not mapped in {path}"
            + (f" (providers: {', '.join(known)})" if known else " (no provider entry)")
            + "; add it to the provider entry (see `aoe-llm-tags setup --print`)",
        )
    return Check(name, OK, f"{path} → {', '.join(notes)}")


def _check_env_mapping(
    adapter: adapters_module.Adapter,
    item: resolver.ResolvedHeader,
    env: Mapping[str, str],
) -> Check:
    name = f"mapping {item.header}"
    variable = "ANTHROPIC_CUSTOM_HEADERS" if adapter.agent == "claude" else ""
    if not variable:
        return Check(name, SKIP, f"adapter {adapter.agent} has no variable check yet")
    value = env.get(variable, "")
    if not value:
        return Check(name, FAIL, f"${variable} is not set; {item.header} is not sent")
    if item.header.lower() not in value.lower():
        return Check(name, FAIL, f"${variable} does not carry {item.header}: {value!r}")
    return Check(name, OK, f"${variable}")


def _check_values(snap: resolver.Snapshot) -> Check:
    if not snap.resolved:
        return Check("value rendering", FAIL, "no header is enabled, so the hook prints nothing")
    broken = [warning for warning in snap.warnings if "unknown field" in warning]
    if broken:
        return Check("value rendering", FAIL, broken[0])
    rendered = "; ".join(
        f"{item.header}={item.env_var}={item.value!r}" for item in snap.resolved
    )
    if all(not item.value for item in snap.resolved):
        return Check(
            "value rendering",
            WARN,
            f"{rendered} — every value is empty (open decision #9: mint empty or emit no line)",
        )
    return Check("value rendering", OK, rendered)


# --- helpers ----------------------------------------------------------------


def _scan_file(
    adapter: adapters_module.Adapter, path: Path, header: str
) -> tuple[bool, list[str], list[str]]:
    """Is ``header`` mapped here? Returns (found, matched provider notes, all ids)."""
    wanted = header.lower()
    data = adapters_module.read_config(adapter, path)
    found, notes, known = False, [], []
    for provider in adapters_module.providers(adapter, data):
        known.append(provider.id)
        values = {str(key).lower(): value for key, value in provider.headers.items()}
        if wanted not in values:
            continue
        found = True
        detail = f"{provider.id} (value: {values[wanted]}"
        if provider.models:
            detail += f", models: {provider.models}"
        notes.append(detail + ")")
    return found, notes, known


def _resolve_adapter(
    flags: resolver.Flags,
    tool: str,
    adapters: Sequence[adapters_module.Adapter],
) -> adapters_module.Adapter | None:
    if flags.adapter:
        return adapters_module.find(adapters, flags.adapter)
    return adapters_module.match_tool(adapters, tool)


def _command_path(entry: str) -> str:
    for token in shlex.split(entry):
        name, sep, _ = token.partition("=")
        if not sep:
            return token
    return ""


def _files(config: settings.RawConfig) -> str:
    return ", ".join(config.sources or config.present) or "(no config file)"


def _abbreviate(text: str, limit: int = 96) -> str:
    return text if len(text) <= limit else text[: limit - 1] + "…"
