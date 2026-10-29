"""Harness adapter data (``adapters.toml``): where a harness keeps its mapping.

The table is data, not code, so a user can fix or add a row without a plugin
release (``docs/harness-adapters.md``). This module only finds a row, resolves
its config path, and renders its mapping line; it never writes anything.

Which system finally reads the header is not an adapter fact
(``docs/header-contract.md``), so nothing here mentions a vendor product.
"""

from __future__ import annotations

import json
import os
import re
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable, Mapping, Sequence

ADAPTERS_FILENAME = "adapters.toml"

_VAR_RE = re.compile(r"\$\{?([A-Za-z_][A-Za-z0-9_]*)\}?")
_UNSET = "\x00"


@dataclass(frozen=True)
class Adapter:
    """One harness row, straight from the table."""

    agent: str
    label: str = ""
    mechanism: str = "unknown"
    tools: tuple[str, ...] = ()
    config_env: str = ""
    config_paths: tuple[str, ...] = ()
    config_format: str = "none"
    mapping_format: str = ""
    status: str = "needs_research"
    notes: str = ""


def plugin_root() -> Path:
    """The plugin directory (``src/llm_tags/adapters.py`` → up three)."""
    return Path(__file__).resolve().parents[2]


def load(
    root: Path | None = None,
) -> tuple[tuple[Adapter, ...], tuple[str, ...]]:
    """Read every adapter row. Missing or broken file = no rows, plus a warning."""
    path = (root or plugin_root()) / ADAPTERS_FILENAME
    try:
        with path.open("rb") as handle:
            table = tomllib.load(handle)
    except FileNotFoundError:
        return (), (f"{path} is missing; no harness adapter is known",)
    except (OSError, tomllib.TOMLDecodeError) as exc:
        return (), (f"{path} could not be read: {exc}",)

    rows = table.get("adapter")
    if not isinstance(rows, list):
        return (), (f"{path} has no [[adapter]] rows",)
    adapters = tuple(row for row in (_parse_row(item) for item in rows) if row is not None)
    return adapters, ()


def find(adapters: Sequence[Adapter], name: str) -> Adapter | None:
    """Match an ``--adapter`` value: the row's agent id, its label, or one of its tools."""
    wanted = (name or "").strip().lower()
    if not wanted:
        return None
    for adapter in adapters:
        if wanted == adapter.agent.lower() or wanted == adapter.label.lower():
            return adapter
    for adapter in adapters:
        if wanted in (tool.lower() for tool in adapter.tools):
            return adapter
    return None


def match_tool(adapters: Sequence[Adapter], tool: str) -> Adapter | None:
    """Match an aoe ``tool`` value: exact tool, then adapter id, then ``pi-*`` → ``pi``."""
    wanted = (tool or "").strip().lower()
    if not wanted:
        return None
    adapter = find(adapters, wanted)
    if adapter is not None:
        return adapter
    for candidate in adapters:
        if wanted.startswith(candidate.agent.lower() + "-"):
            return candidate
    return None


def expand_path(path: str, env: Mapping[str, str] | None = None) -> Path | None:
    """Expand ``~`` and ``$VAR`` from ``env`` (never the ambient process).

    ``None`` when a variable the path needs is unset: a half-expanded path is not
    a path, and guessing one would write to the wrong file.
    """
    env = os.environ if env is None else env
    if not path:
        return None
    home = env.get("HOME") or os.path.expanduser("~")
    if path == "~" or path.startswith("~/"):
        path = home + path[1:]
    expanded = _VAR_RE.sub(lambda match: env.get(match.group(1), _UNSET), path)
    if _UNSET in expanded:  # an unset variable stayed literal
        return None
    return Path(expanded)


def config_path(
    adapter: Adapter,
    env: Mapping[str, str] | None = None,
) -> tuple[Path | None, tuple[str, ...]]:
    """The first ``config_paths`` entry that resolves — the write/check target.

    Preference order is adapter data on purpose: a launcher that regenerates a
    directory must list its *source* file first, so the generated copy is never
    the target (``docs/harness-adapters.md``).
    """
    warnings: list[str] = []
    for candidate in adapter.config_paths:
        resolved = expand_path(candidate, env)
        if resolved is not None:
            return resolved, ()
    if adapter.config_paths:
        warnings.append(
            f"adapter {adapter.agent}: none of its config paths resolve "
            f"({', '.join(adapter.config_paths)})"
        )
    return None, tuple(warnings)


def mapping_line(
    adapter: Adapter,
    *,
    header: str,
    env_var: str,
    env: Mapping[str, str] | None = None,
) -> str | None:
    """One printable mapping line for one header (``None`` when the row has no format)."""
    path, _ = config_path(adapter, env)
    if not adapter.mapping_format or path is None:
        return None
    provider = provider_id(adapter, path) or "<provider-id>"
    return adapter.mapping_format.format(
        file=str(path), header=header, env_var=env_var, provider=provider
    )


def provider_id(adapter: Adapter, path: Path) -> str:
    """The provider entry to name in a printed line, when the config already says.

    Only a config the user already has can be named; otherwise the line carries
    ``<provider-id>`` rather than guessing an id the harness does not use. For pi
    the entry must have a non-empty ``models`` list, because a launcher that
    rebuilds the provider drops a headers-only entry.
    """
    try:
        data = read_config(adapter, path)
    except Exception:
        return ""
    rows = providers(adapter, data)
    if adapter.agent == "pi":
        usable = [row for row in rows if row.models > 0]
        rows = usable
    return rows[0].id if len(rows) == 1 else ""


@dataclass(frozen=True)
class Provider:
    """One provider entry and the header table inside it."""

    id: str
    headers: Mapping[str, Any]
    models: int = 0


def read_config(adapter: Adapter, path: Path) -> Any:
    """Parse a harness config file according to its declared format."""
    if adapter.config_format == "json":
        return json.loads(path.read_text(encoding="utf-8"))
    if adapter.config_format == "toml":
        return tomllib.loads(path.read_text(encoding="utf-8"))
    raise ValueError(f"adapter {adapter.agent} has no readable config format")


def providers(adapter: Adapter, data: Any) -> list[Provider]:
    """Every provider entry the adapter's format defines, with its header table."""
    if not isinstance(data, Mapping):
        return []
    if adapter.agent == "codex":
        root = data.get("model_providers")
        key = "env_http_headers"
    elif adapter.agent == "pi":
        root = data.get("providers")
        key = "headers"
    elif adapter.agent == "opencode":
        root = data.get("provider")
        key = "options"
    else:
        return []
    if not isinstance(root, Mapping):
        return []

    rows: list[Provider] = []
    for raw_id, value in root.items():
        if not isinstance(value, Mapping):
            continue
        if adapter.agent == "opencode":
            options = value.get("options")
            value = options if isinstance(options, Mapping) else {}
        headers = value.get(key)
        if not isinstance(headers, Mapping):
            headers = {}
        models = value.get("models")
        rows.append(
            Provider(
                id=str(raw_id),
                headers=headers,
                models=len(models) if isinstance(models, list) else 0,
            )
        )
    return rows


def _parse_row(item: Any) -> Adapter | None:
    if not isinstance(item, Mapping):
        return None
    agent = item.get("agent")
    if not isinstance(agent, str) or not agent:
        return None
    return Adapter(
        agent=agent,
        label=_str(item.get("label")) or agent,
        mechanism=_str(item.get("mechanism")) or "unknown",
        tools=_str_tuple(item.get("tools")) or (agent,),
        config_env=_str(item.get("config_env")),
        config_paths=_str_tuple(item.get("config_paths")),
        config_format=_str(item.get("config_format")) or "none",
        mapping_format=_str(item.get("mapping_format")),
        status=_str(item.get("status")) or "needs_research",
        notes=_str(item.get("notes")),
    )


def _str(value: Any) -> str:
    return value if isinstance(value, str) else ""


def _str_tuple(value: Any) -> tuple[str, ...]:
    if not isinstance(value, Iterable) or isinstance(value, (str, bytes)):
        return ()
    return tuple(item for item in value if isinstance(item, str))
