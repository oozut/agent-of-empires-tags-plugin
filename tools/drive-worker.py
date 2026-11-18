#!/usr/bin/env python3
"""Drive the plugin worker the way the daemon does, and show what it pushes.

The dashboard has one surface the worker feeds — the session pane — and an edit
path that runs the other way: a pane row → `POST /api/plugins/<id>/action` → a
JSON-RPC notification to the worker. Neither side needs a browser to check: the
worker is a stdio peer, and this script plays the host side of that conversation
(answering `sessions.list` and `config.get`, recording `ui.state.set`), so the
exact payloads the dashboard would render can be read as JSON.

Usage:

    tools/drive-worker.py                 # publish once, print the pushes
    tools/drive-worker.py --slot pane     # only one slot, less noise
    tools/drive-worker.py --edit tag.set --param key=team --param value=core

`--edit` sends a worker method the way a click does: the method name **verbatim**
(no `plugin.<id>.` prefix — the host forwards the block's `method` as written) and
the session id a pane row would pass, which the host overwrites with the one it
trusts.

The session list comes from a real `aoe list --json`, but the config tree can be
a throwaway (`XDG_CONFIG_HOME`), so an experiment never edits real settings and
never writes an override into a live session's directory.
"""

import argparse
import json
import os
import queue
import subprocess
import sys
import threading
import tomllib

PLUGIN = "dev.oozut.llm-tags"
WORKER_NAME = "aoe-llm-tags-worker"


def parse_args():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--worker",
        help="worker binary (default: the installed plugin's)",
    )
    parser.add_argument(
        "--config-home",
        help="XDG_CONFIG_HOME holding an agent-of-empires tree (default: the real one)",
    )
    parser.add_argument(
        "--slot",
        choices=["row-badge", "pane"],
        help="print only this slot",
    )
    parser.add_argument("--limit", type=int, default=4000, help="bytes per payload")
    parser.add_argument(
        "--edit",
        metavar="METHOD",
        help="also fire this worker method (pane.tab, tag.set, tag.remove, "
        "tag.level.unset, tags.reset)",
    )
    parser.add_argument(
        "--param",
        action="append",
        default=[],
        metavar="KEY=VALUE",
        help="with --edit: one parameter (repeatable; the value is JSON if it parses)",
    )
    parser.add_argument(
        "--session",
        help="with --edit: the session id to act on (default: the only live one)",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=3.0,
        help="seconds of worker silence that end the run",
    )
    return parser.parse_args()


def default_worker():
    root = os.environ.get("AOE_CONFIG_HOME") or os.path.join(
        os.environ.get("XDG_CONFIG_HOME", os.path.join(os.environ["HOME"], ".config")),
        "agent-of-empires",
    )
    return os.path.join(root, "plugins", PLUGIN, "bin", WORKER_NAME)


def read_settings(config_home):
    """The plugin's settings, read the way the daemon answers `config.get`."""
    root = os.path.join(config_home, "agent-of-empires")
    path = os.path.join(root, "config.toml")
    try:
        with open(path, "rb") as handle:
            config = tomllib.load(handle)
    except OSError:
        return {}
    return config.get("plugins", {}).get(PLUGIN, {}).get("settings", {})


def live_sessions():
    """The session list, as the host supplies it — from the *real* daemon, even
    when the worker under test runs against a throwaway config tree."""
    result = subprocess.run(
        ["aoe", "list", "--json"],
        capture_output=True,
        text=True,
        env={**os.environ, "XDG_CONFIG_HOME": os.path.expanduser("~/.config")},
    )
    if result.returncode != 0:
        raise SystemExit(f"aoe list failed: {result.stderr.strip()}")
    rows = json.loads(result.stdout or "[]")
    rows = rows.get("sessions", rows) if isinstance(rows, dict) else rows
    return [
        {
            "id": row.get("id"),
            "title": row.get("title") or "",
            "tool": row.get("tool") or "",
            "project_path": row.get("path") or row.get("project_path") or "",
            "group": row.get("group") or "",
            "profile": row.get("profile") or "",
            "branch": (row.get("worktree") or {}).get("branch") or "",
        }
        for row in rows
    ]


def coerce(text):
    try:
        return json.loads(text)
    except ValueError:
        return text


def main():
    args = parse_args()
    worker = args.worker or default_worker()
    config_home = args.config_home or os.environ.get(
        "XDG_CONFIG_HOME", os.path.expanduser("~/.config")
    )
    settings = read_settings(config_home)
    sessions = live_sessions()

    env = dict(os.environ)
    env["XDG_CONFIG_HOME"] = config_home
    proc = subprocess.Popen(
        [worker],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
        env=env,
    )

    def send(value):
        proc.stdin.write(json.dumps(value) + "\n")
        proc.stdin.flush()

    lines = queue.Queue()

    def read():
        for line in proc.stdout:
            lines.put(line)
        lines.put(None)

    threading.Thread(target=read, daemon=True).start()

    # One host event is what makes the worker publish every surface; it is the
    # same notification the daemon sends when a session's status moves.
    send({"jsonrpc": "2.0", "method": "session.status.changed", "params": {}})
    if args.edit:
        params = dict(pair.split("=", 1) for pair in args.param)
        params = {key: coerce(value) for key, value in params.items()}
        session = args.session or "".join(
            row["id"] for row in sessions if row["id"] == params.get("session_id")
        )
        if not session:
            if len(sessions) != 1:
                raise SystemExit(
                    "--edit needs --session: "
                    f"{len(sessions)} sessions are live, so there is no obvious one"
                )
            session = sessions[0]["id"]
        params["session_id"] = session
        # Verbatim, exactly as the host forwards a pane block's `method`.
        send({"jsonrpc": "2.0", "method": args.edit, "params": params})

    pushes = []
    answered = 0
    while True:
        try:
            line = lines.get(timeout=args.timeout)
        except queue.Empty:
            break
        if line is None:
            break
        line = line.strip()
        if not line:
            continue
        message = json.loads(line)
        method = message.get("method")
        if method is None:
            print("ANSWERED-BY-WORKER", json.dumps(message)[:400], flush=True)
            continue
        if method == "sessions.list":
            result = {"sessions": sessions}
        elif method == "config.get":
            key = (message.get("params") or {}).get("key")
            result = {"value": settings.get(key)}
        elif method == "ui.state.set":
            pushes.append(message["params"])
            result = {"ok": True}
        else:
            result = {}
        send({"jsonrpc": "2.0", "id": message["id"], "result": result})
        answered += 1

    proc.kill()
    error = (proc.stderr.read() or "").strip()
    if error:
        print("STDERR:", error[:2000], file=sys.stderr)

    shown = 0
    for push in pushes:
        if args.slot and push.get("slot") != args.slot:
            continue
        shown += 1
        print(
            f"PUSH {push.get('slot')} {push.get('id')} {push.get('session_id') or '(global)'}",
            flush=True,
        )
        print(json.dumps(push.get("payload"), indent=1)[: args.limit], flush=True)
    print(f"-- {shown} of {len(pushes)} pushes shown, {answered} host calls answered")


main()
