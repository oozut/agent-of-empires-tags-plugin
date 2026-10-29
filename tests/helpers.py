"""Fixtures shared by the tests: a temp config tree, a fake aoe, a fake harness."""

from __future__ import annotations

import json
import os
import stat
from pathlib import Path
from typing import Any, Mapping, Sequence

from llm_tags import resolver

PLUGIN_ID = "dev.oozut.llm-tags"


def app_env(tmp: Path, **extra: str) -> dict[str, str]:
    """A minimal environment: temp app dir + temp HOME, nothing inherited."""
    env = {
        "HOME": str(tmp / "home"),
        "XDG_CONFIG_HOME": str(tmp / "config"),
        "PATH": "/usr/local/bin:/usr/bin:/bin",
    }
    env.update(extra)
    Path(env["HOME"]).mkdir(parents=True, exist_ok=True)
    return env


def write_config(
    env: Mapping[str, str],
    body: str,
    *,
    profile: str = "",
) -> Path:
    """Write ``config.toml`` (or the profile override) under the env's app dir."""
    root = Path(env["XDG_CONFIG_HOME"]) / "agent-of-empires"
    path = root / (f"profiles/{profile}/config.toml" if profile else "config.toml")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body, encoding="utf-8")
    return path


def settings_config(settings: str, *, hooks: str = "") -> str:
    """A config with a plugin settings block and optional ``[host_hooks]``."""
    parts = [f'[plugins."{PLUGIN_ID}".settings]\n{settings.strip()}\n']
    if hooks:
        parts.append(f"[host_hooks]\n{hooks.strip()}\n")
    return "\n".join(parts)


def write_executable(path: Path, body: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body, encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return path


def write_json(path: Path, data: Any) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    return path


def pi_models(
    path: Path,
    *,
    provider: str = "litellm",
    headers: Mapping[str, str] | None = None,
    models: Sequence[Mapping[str, str]] | None = None,
) -> Path:
    """A pi ``models.json`` shaped the way this host's launcher requires it."""
    return write_json(
        path,
        {
            "providers": {
                provider: {
                    "baseUrl": "http://127.0.0.1:8787/v1",
                    "api": "openai-completions",
                    "apiKey": "$LITELLM_API_KEY",
                    "headers": dict(headers or {}),
                    "models": list(
                        models if models is not None else [{"id": "deepseek-v4-flash"}]
                    ),
                }
            }
        },
    )


class FakeAoe:
    """A runner that answers like ``aoe`` and records every argv it was given."""

    def __init__(
        self,
        session: Mapping[str, Any] | None = None,
        rows: Sequence[Mapping[str, Any]] | None = None,
        error: str = "",
    ) -> None:
        self.session = dict(session or {})
        self.rows = list(rows or [])
        self.error = error
        self.calls: list[list[str]] = []

    def __call__(self, argv: Sequence[str]) -> Any:
        self.calls.append(list(argv))
        if self.error:
            raise resolver.AoeError(self.error)
        if argv[0] == "session":
            return dict(self.session)
        if argv[0] == "list":
            return list(self.rows)
        raise AssertionError(f"unexpected aoe call: {argv}")

    @property
    def subcommands(self) -> list[str]:
        return [call[0] for call in self.calls]


def fake_aoe_script(path: Path, session: Mapping[str, Any] | None = None) -> Path:
    """A real executable that answers ``session show``/``list``, for CLI-level tests."""
    payload = json.dumps(dict(session or {}))
    return write_executable(
        path,
        "#!/bin/sh\n"
        'case "$1" in\n'
        f"  session) echo '{payload}' ;;\n"
        "  list) echo '[]' ;;\n"
        "  *) echo '{}' ;;\n"
        "esac\n",
    )
