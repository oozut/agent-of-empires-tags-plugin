"""Argument parsing and the M0 verbs implemented so far: ``hook`` and ``value``.

The whole user-visible contract for these two (``plan/parameters.md`` §6):
stdout is machine-readable only — one ``NAME=value`` line per enabled header for
``hook``, one value for ``value`` — and diagnostics go to stderr. Exit codes are
0 for ok/degraded, 1 for a failing check, 2 for a usage error. A hook must never
abort a session start, so an unexpected failure is still exit 0.

``doctor`` (M0-T11), ``setup --print`` (M0-T05) and ``verify`` (M2) are named but
not implemented here; they report which task owns them rather than pretending to
be a different error.
"""

from __future__ import annotations

import argparse
import sys
import time
from typing import Mapping, Sequence

from . import resolver, settings

PROG = "aoe-llm-tags"
EXIT_OK = 0
EXIT_FAIL = 1
EXIT_USAGE = 2

NOT_IMPLEMENTED = {
    "doctor": "M0-T11 (plan/m0.md)",
    "setup": "M0-T05 (plan/m0.md)",
    "verify": "M2 (plan/m2.md)",
}


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog=PROG,
        description="Resolve this session's tag headers (Agent of Empires plugin).",
    )
    commands = parser.add_subparsers(dest="command", required=True, metavar="<command>")

    hook = commands.add_parser(
        "hook",
        help="print one NAME=value per enabled header",
        description="Mint the header variables for this launch. Used by both [host_hooks] keys.",
    )
    _add_flags(hook)

    value = commands.add_parser(
        "value",
        help="print one header's value",
        description="Print a single header's value, for harnesses that take a command.",
    )
    value.add_argument(
        "header",
        nargs="?",
        default=resolver.DEFAULT_HEADER,
        help=f"header name (default: {resolver.DEFAULT_HEADER})",
    )
    _add_flags(value)

    for name, owner in NOT_IMPLEMENTED.items():
        commands.add_parser(name, help=f"not implemented yet — {owner}")

    return parser


def _add_flags(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "--session",
        help="session id fallback (default: $AOE_SESSION_ID); the session row wins",
    )
    parser.add_argument(
        "--profile",
        help="profile fallback (default: $AOE_PROFILE); selects the profile config",
    )
    parser.add_argument("--tool", help="tool fallback (default: $AOE_TOOL)")
    parser.add_argument("--project", help="project path fallback (default: $AOE_PROJECT_PATH)")
    parser.add_argument("--group", help="group fallback (default: $AOE_GROUP_PATH)")
    parser.add_argument("--title", help="title fallback (default: $AOE_SESSION_TITLE)")
    parser.add_argument(
        "--adapter",
        help="harness adapter row to pin instead of detecting (used by setup/doctor from M0-T05)",
    )


def main(
    argv: Sequence[str] | None = None,
    env: Mapping[str, str] | None = None,
) -> int:
    _soften_stdout()
    argv = list(sys.argv[1:] if argv is None else argv)

    # Named-but-absent verbs answer for themselves, flags and all: `setup --print`
    # exists only in M0-T05, and a bare argparse "unrecognized arguments" would
    # read like a typo instead of a task that has not landed yet.
    if argv and argv[0] in NOT_IMPLEMENTED:
        _warn(f"`{argv[0]}` is not implemented yet; it is {NOT_IMPLEMENTED[argv[0]]}")
        return EXIT_USAGE

    args = build_parser().parse_args(argv)

    if args.command == "hook":
        return _hook(args, env)
    if args.command == "value":
        return _value(args, env)

    _warn(f"`{args.command}` is not implemented yet")
    return EXIT_USAGE


def _hook(args: argparse.Namespace, env: Mapping[str, str] | None) -> int:
    try:
        resolved, warnings = _resolve(args, env)
        lines = resolver.hook_lines(resolved)
    except Exception as exc:  # the hook must never abort a session start
        _warn(f"unexpected error: {exc}")
        return EXIT_OK
    _warn_all(warnings)
    for line in lines:
        print(line)
    return EXIT_OK


def _value(args: argparse.Namespace, env: Mapping[str, str] | None) -> int:
    wanted = args.header
    try:
        resolved, warnings = _resolve(args, env)
    except Exception as exc:
        _warn(f"unexpected error: {exc}")
        return EXIT_OK
    _warn_all(warnings)
    for item in resolved:
        if item.header.lower() == wanted.lower():
            sys.stdout.write(f"{item.value}\n")
            return EXIT_OK
    _warn(
        f"header {wanted!r} is not enabled for this session "
        f"(enabled: {', '.join(item.header for item in resolved) or 'none'})"
    )
    return EXIT_OK


def _resolve(
    args: argparse.Namespace,
    env: Mapping[str, str] | None,
) -> tuple[tuple[resolver.ResolvedHeader, ...], tuple[str, ...]]:
    """The single path every verb takes: flags → facts → settings → render.

    One launch budget covers every subprocess: the row lookup and the optional
    `aoe list` for {branch} share `DEFAULT_TIMEOUT`, so a slow or missing `aoe`
    can delay a launch once, never twice.
    """
    started = time.monotonic()
    flags = resolver.Flags.from_args(args, env)
    facts, session_warnings = resolver.resolve_session(flags, env=env)
    elapsed = time.monotonic() - started
    raw = settings.load(profile=facts.profile or flags.profile, env=env)
    configured = resolver.Settings.from_table(raw.table, warnings=session_warnings + raw.warnings)
    resolved, render_warnings = resolver.render_headers(
        configured,
        facts,
        env=env,
        timeout=max(0.0, resolver.DEFAULT_TIMEOUT - elapsed),
    )
    return resolved, configured.warnings + render_warnings


def _warn(message: str) -> None:
    sys.stderr.write(f"{PROG}: {message}\n")


def _warn_all(messages: Sequence[str]) -> None:
    for message in dict.fromkeys(messages):
        _warn(message)


def _soften_stdout() -> None:
    """Never die on an unencodable template: replace, do not raise."""
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if reconfigure is not None:
            try:
                reconfigure(errors="replace")
            except (ValueError, OSError):  # pragma: no cover - detached streams
                pass
