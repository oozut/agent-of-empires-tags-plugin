"""Where the plugin's settings live, and how a profile overrides them.

aoe stores plugin settings as an opaque table under
``[plugins."dev.oozut.llm-tags".settings]`` in ``<app_dir>/config.toml``
(``settings: toml::Table`` in ``src/session/config/mod.rs``) and keeps them even
while the plugin is disabled. A profile adds ``<app_dir>/profiles/<name>/config.toml``,
whose *top level* is the sparse override tree, and aoe merges it onto the global
config as a JSON merge: object keys recurse, arrays replace
(``profile_config.rs`` / ``merge_configs_generic``). This module reproduces
exactly that for the one section the plugin owns, so the hook and the settings UI
cannot disagree about a value.

Failure behaviour is the hook's, not a library's: a file that cannot be read or
parsed is a warning and an empty table, never an exception. The hook must exit 0
even when the store is broken (``docs/implementation.md`` §Step 1).

Stdlib only, no aoe imports.
"""

from __future__ import annotations

import tomllib
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Mapping

PLUGIN_ID = "dev.oozut.llm-tags"

# Release-build app dir name (src/session/mod.rs APP_DIR_NAME_XDG). Debug builds
# use "agent-of-empires-dev"; a hook cannot tell which build launched it, so it
# reads the release dir, which is what this deployment runs.
APP_DIR_NAME = "agent-of-empires"

GLOBAL_CONFIG = "config.toml"
PROFILE_CONFIG = "config.toml"


def app_dir(env: Mapping[str, str] | None = None) -> Path:
    """The aoe app dir: ``$XDG_CONFIG_HOME`` when absolute, else ``~/.config``.

    Mirrors ``xdg_config_base()`` + ``get_app_dir_path()`` for a Linux release
    build. No aoe env var overrides this, so neither do we.
    """
    env = _env(env)
    xdg = env.get("XDG_CONFIG_HOME")
    if xdg and Path(xdg).is_absolute():
        base = Path(xdg)
    else:
        home = env.get("HOME") or str(Path.home())
        base = Path(home) / ".config"
    return base / APP_DIR_NAME


def global_config_path(root: Path) -> Path:
    return root / GLOBAL_CONFIG


def profile_config_path(root: Path, profile: str) -> Path | None:
    if not profile:
        return None
    return root / "profiles" / profile / PROFILE_CONFIG


@dataclass(frozen=True)
class RawSettings:
    """The merged ``[plugins.<id>.settings]`` table, plus how we got it.

    ``table`` is unvalidated TOML-shaped data: normalising it into headers and
    rules is the resolver's job, so this module never has an opinion about what
    a valid header is.
    """

    table: dict[str, Any] = field(default_factory=dict)
    warnings: tuple[str, ...] = ()
    sources: tuple[str, ...] = ()


def load(
    profile: str = "",
    *,
    root: Path | None = None,
    env: Mapping[str, str] | None = None,
) -> RawSettings:
    """Load the effective plugin settings for ``profile`` (empty = global only).

    Reads the global config, then the profile's override file, and returns the
    merged plugin table. A missing file is not a warning; an unreadable or
    malformed one is, and contributes nothing.
    """
    root = root or app_dir(env)
    warnings: list[str] = []
    sources: list[str] = []

    config: dict[str, Any] = {}
    path = global_config_path(root)
    loaded = _read_table(path, warnings)
    if loaded is not None:
        sources.append(str(path))
        config = loaded

    profile_path = profile_config_path(root, profile)
    if profile_path is not None:
        overrides = _read_table(profile_path, warnings)
        if overrides is not None:
            sources.append(str(profile_path))
            config = deep_merge(config, overrides)

    return RawSettings(table=plugin_table(config), warnings=tuple(warnings), sources=tuple(sources))


def _read_table(path: Path, warnings: list[str]) -> dict[str, Any] | None:
    """Read one TOML file as a table; ``None`` when it is absent or unusable."""
    try:
        raw = path.read_text(encoding="utf-8")
    except FileNotFoundError:
        return None
    except OSError as exc:
        warnings.append(f"{path}: cannot be read ({exc}); ignoring it")
        return None
    try:
        parsed = tomllib.loads(raw)
    except tomllib.TOMLDecodeError as exc:
        warnings.append(f"{path}: malformed TOML ({exc}); ignoring it")
        return None
    if not isinstance(parsed, dict):  # pragma: no cover - tomllib always yields a table
        warnings.append(f"{path}: not a TOML table; ignoring it")
        return None
    return parsed


def deep_merge(base: Mapping[str, Any], override: Mapping[str, Any]) -> dict[str, Any]:
    """JSON-merge semantics: keys recurse, scalars **and arrays** replace."""
    merged = dict(base)
    for key, value in override.items():
        current = merged.get(key)
        if isinstance(value, Mapping) and isinstance(current, Mapping):
            merged[key] = deep_merge(current, value)
        else:
            merged[key] = value
    return merged


def plugin_table(config: Mapping[str, Any]) -> dict[str, Any]:
    """``[plugins.<PLUGIN_ID>.settings]`` as a plain dict (empty when absent)."""
    plugins = config.get("plugins")
    if not isinstance(plugins, Mapping):
        return {}
    entry = plugins.get(PLUGIN_ID)
    if not isinstance(entry, Mapping):
        return {}
    settings = entry.get("settings")
    if not isinstance(settings, Mapping):
        return {}
    return dict(settings)


def _env(env: Mapping[str, str] | None) -> Mapping[str, str]:
    if env is not None:
        return env
    import os

    return os.environ
