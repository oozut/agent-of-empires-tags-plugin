"""``setup --print`` (M0-T05): the exact lines a user pastes, and nothing else.

Two host-hook lines (one per ``[host_hooks]`` key — installing one key only tags
one substrate) plus one mapping line per enabled header for the selected harness.
``--print`` never writes: applying the mapping is a later, explicit action
(``docs/harness-adapters.md`` §Auto-discovery).

Absolute paths everywhere, resolved once: a hook line that needs the user's
``PATH`` to mean the same thing inside a container is not reproducible.
"""

from __future__ import annotations

import os
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Mapping, Sequence

from . import adapters as adapters_module
from . import resolver

HOOK_KEYS = ("before_session", "before_start")


class SetupError(RuntimeError):
    """A setup request that cannot be answered honestly (no adapter, no binary)."""


@dataclass(frozen=True)
class Mapping:
    """One printable mapping line: what to add, where, and for which variable."""

    header: str
    env_var: str
    file: str
    line: str


@dataclass(frozen=True)
class Plan:
    adapter: adapters_module.Adapter | None
    cli: Path
    aoe: str
    hooks: dict[str, str] = field(default_factory=dict)
    mapping: tuple[Mapping, ...] = ()
    warnings: tuple[str, ...] = ()


def cli_path(root: Path | None = None) -> Path:
    """This plugin's ``bin/aoe-llm-tags`` (absolute, whatever the install location)."""
    return ((root or adapters_module.plugin_root()) / "bin" / "aoe-llm-tags").resolve()


def hook_command(cli: Path, aoe: str) -> str:
    """The one command installed under both ``[host_hooks]`` keys.

    Every ``AOE_*`` value the hook reads is passed explicitly, so the command works
    in the reduced environment structured host sessions get, and ``AOE_BIN`` pins
    the binary the plugin resolved instead of whatever ``PATH`` happens to hold.
    """
    return (
        f'AOE_BIN="{aoe}" "{cli}" hook'
        ' --session "$AOE_SESSION_ID"'
        ' --profile "$AOE_PROFILE"'
        ' --tool "$AOE_TOOL"'
        ' --project "$AOE_PROJECT_PATH"'
        ' --group "$AOE_GROUP_PATH"'
        ' --title "$AOE_SESSION_TITLE"'
    )


def build(
    flags: resolver.Flags | None = None,
    *,
    env: Mapping[str, str] | None = None,
    root: Path | None = None,
    adapters: Sequence[adapters_module.Adapter] | None = None,
) -> Plan:
    """Resolve everything ``--print`` needs, or raise :class:`SetupError`.

    Uses the same snapshot as ``hook``, so the mapped headers are exactly the ones
    this launch would emit; a header the resolver refuses cannot be mapped.
    """
    env = os.environ if env is None else env
    cli = cli_path(root)
    aoe = resolver.aoe_bin(env)
    warnings: list[str] = []

    if not os.access(cli, os.X_OK):
        raise SetupError(
            f"{cli} is missing or not executable; run `aoe plugin update dev.oozut.llm-tags`"
        )
    if not (os.access(aoe, os.X_OK) or os.path.isdir(aoe)):
        raise SetupError(
            f"the aoe binary {aoe} is not executable; set AOE_BIN to this host's aoe"
        )

    if adapters is None:
        adapters, adapter_warnings = adapters_module.load(root)
        warnings.extend(adapter_warnings)

    snap = resolver.snapshot(flags, env=env)
    warnings.extend(snap.warnings)

    adapter = _adapter_for(snap.facts.tool, flags, adapters)
    if adapter is None:
        known = ", ".join(sorted(row.agent for row in adapters)) or "none"
        raise SetupError(
            f"no adapter for tool {snap.facts.tool!r}; pass --adapter <agent> "
            f"(known: {known})"
        )
    if not adapter.mapping_format:
        raise SetupError(
            f"adapter {adapter.agent} is {adapter.status} and has no mapping format yet; "
            "no static setup can be printed for it"
        )

    mapping: list[Mapping] = []
    for item in snap.resolved:
        line = adapters_module.mapping_line(
            adapter, header=item.header, env_var=item.env_var, env=env
        )
        if line is None:
            warnings.append(
                f"adapter {adapter.agent}: no config path resolves; cannot map {item.header}"
            )
            continue
        path, _ = adapters_module.config_path(adapter, env)
        mapping.append(
            Mapping(
                header=item.header,
                env_var=item.env_var,
                file=str(path),
                line=line,
            )
        )

    command = hook_command(cli, aoe)
    return Plan(
        adapter=adapter,
        cli=cli,
        aoe=aoe,
        hooks={key: command for key in HOOK_KEYS},
        mapping=tuple(mapping),
        warnings=tuple(warnings),
    )


def text(plan: Plan) -> list[str]:
    """The printable lines: two hook lines, then one line per enabled header."""
    lines = [f"{key} = '{plan.hooks[key]}'" for key in HOOK_KEYS]
    lines.extend(item.line for item in plan.mapping)
    return lines


def as_dict(plan: Plan) -> dict[str, Any]:
    return {
        "adapter": plan.adapter.agent if plan.adapter else "",
        "aoe": plan.aoe,
        "cli": str(plan.cli),
        "hooks": dict(plan.hooks),
        "mapping": [
            {
                "header": item.header,
                "env_var": item.env_var,
                "file": item.file,
                "line": item.line,
            }
            for item in plan.mapping
        ],
        "warnings": list(plan.warnings),
    }


def _adapter_for(
    tool: str,
    flags: resolver.Flags | None,
    adapters: Sequence[adapters_module.Adapter],
) -> adapters_module.Adapter | None:
    pinned = getattr(flags, "adapter", "") if flags is not None else ""
    if pinned:
        return adapters_module.find(adapters, pinned)
    return adapters_module.match_tool(adapters, tool)
