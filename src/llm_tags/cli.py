"""Argument parsing and the M0 verbs: ``hook``, ``value``, ``setup --print``, ``doctor``.

The user-visible contract for the first two (``plan/parameters.md`` §6): stdout is
machine-readable only — one ``NAME=value`` line per enabled header for ``hook``,
one value for ``value`` — and diagnostics go to stderr. Exit codes are 0 for
ok/degraded, 1 for a failing check, 2 for a usage error. A hook must never abort a
session start, so an unexpected failure there is still exit 0.

``verify`` (M2) is named but not implemented: it reports which milestone owns it
rather than pretending to be a different error.
"""

from __future__ import annotations

import argparse
import json
import sys
from typing import Mapping, Sequence

from . import doctor, resolver, setup

PROG = "aoe-llm-tags"
EXIT_OK = 0
EXIT_FAIL = 1
EXIT_USAGE = 2

NOT_IMPLEMENTED = {
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

    setup_cmd = commands.add_parser(
        "setup",
        help="print the hook and header-mapping lines to paste",
        description=(
            "Print the two [host_hooks] lines and one mapping line per enabled header. "
            "Print-only: nothing is written."
        ),
    )
    setup_cmd.add_argument(
        "--print",
        action="store_true",
        help="print the lines (the only mode implemented so far)",
    )
    setup_cmd.add_argument("--json", action="store_true", help="machine-readable output")
    _add_flags(setup_cmd)

    doctor_cmd = commands.add_parser(
        "doctor",
        help="check the wiring; exit 1 if anything would silently send nothing",
        description="One line per check, with the absolute paths it looked at.",
    )
    doctor_cmd.add_argument("--json", action="store_true", help="machine-readable output")
    _add_flags(doctor_cmd)

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
        help="harness adapter row (pi, claude, codex, …) instead of detecting it from the tool",
    )


def main(
    argv: Sequence[str] | None = None,
    env: Mapping[str, str] | None = None,
) -> int:
    _soften_stdout()
    argv = list(sys.argv[1:] if argv is None else argv)

    # A named-but-absent verb answers for itself, flags and all: a bare argparse
    # "unrecognized arguments" would read like a typo instead of a future task.
    if argv and argv[0] in NOT_IMPLEMENTED:
        _warn(f"`{argv[0]}` is not implemented yet; it is {NOT_IMPLEMENTED[argv[0]]}")
        return EXIT_USAGE

    args = build_parser().parse_args(argv)

    if args.command == "hook":
        return _hook(args, env)
    if args.command == "value":
        return _value(args, env)
    if args.command == "setup":
        return _setup(args, env)
    if args.command == "doctor":
        return _doctor(args, env)

    _warn(f"`{args.command}` is not implemented yet")
    return EXIT_USAGE


def _hook(args: argparse.Namespace, env: Mapping[str, str] | None) -> int:
    try:
        snap = _snapshot(args, env)
        lines = resolver.hook_lines(snap.resolved)
    except Exception as exc:  # the hook must never abort a session start
        _warn(f"unexpected error: {exc}")
        return EXIT_OK
    _warn_all(snap.warnings)
    for line in lines:
        print(line)
    return EXIT_OK


def _value(args: argparse.Namespace, env: Mapping[str, str] | None) -> int:
    wanted = args.header
    try:
        snap = _snapshot(args, env)
    except Exception as exc:
        _warn(f"unexpected error: {exc}")
        return EXIT_OK
    _warn_all(snap.warnings)
    for item in snap.resolved:
        if item.header.lower() == wanted.lower():
            sys.stdout.write(f"{item.value}\n")
            return EXIT_OK
    _warn(
        f"header {wanted!r} is not enabled for this session "
        f"(enabled: {', '.join(item.header for item in snap.resolved) or 'none'})"
    )
    return EXIT_OK


def _setup(args: argparse.Namespace, env: Mapping[str, str] | None) -> int:
    if not args.print and not args.json:
        _warn("`setup` is print-only in M0; pass --print (or --json)")
        return EXIT_USAGE
    flags = resolver.Flags.from_args(args, env)
    try:
        plan = setup.build(flags, env=env)
    except setup.SetupError as exc:
        _warn(str(exc))
        return EXIT_FAIL
    _warn_all(plan.warnings)
    if args.json:
        print(json.dumps(setup.as_dict(plan), indent=2))
    else:
        for line in setup.text(plan):
            print(line)
    return EXIT_OK


def _doctor(args: argparse.Namespace, env: Mapping[str, str] | None) -> int:
    flags = resolver.Flags.from_args(args, env)
    report = doctor.run(flags, env=env)
    print(doctor.format_json(report) if args.json else doctor.format_text(report))
    if report.ok:
        return EXIT_OK
    failed = [check.name for check in report.checks if check.status == doctor.FAIL]
    _warn(f"{len(failed)} check(s) failed: {', '.join(failed)}")
    return EXIT_FAIL


def _snapshot(args: argparse.Namespace, env: Mapping[str, str] | None) -> resolver.Snapshot:
    """The single path every verb takes: flags → facts → settings → render."""
    return resolver.snapshot(resolver.Flags.from_args(args, env), env=env)


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
