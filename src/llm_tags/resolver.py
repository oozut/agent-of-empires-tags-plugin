"""The one resolver: session facts → fields → one value per enabled header.

Every verb (`hook`, `value`, and from M1 `doctor`/`status`) calls this module, so
they cannot drift. The only outside dependency is a subprocess call to
``aoe session show --json`` / ``aoe list --json``, always behind an injectable
runner and a hard timeout: the tests need no aoe, no network, no daemon.

Resolution order per field is fixed by the plan (``plan/m0.md`` task 2, lane
brief ``plan/lanes/m0-core.md``): the session row first, then the flags the hook
filled from ``AOE_*``, then defaults. The row is a superset of the hook
environment and the only source of ``group``/``title`` for structured host
sessions.

Values follow ``plan/parameters.md`` §4 (template fields) and §5 (value rules).
Empty results are still emitted — one line per enabled header is the output
contract, and open decision #9 stays open until a consumer is chosen.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Iterable, Mapping, Sequence

from . import settings as settings_module

# --- constants: plan/parameters.md §4, §5, §7 -------------------------------

TAG_CAP = 5
TAG_MAX_CHARS = 32
VALUE_MAX_BYTES = 800
SHORT_ID_PREFIX = "S-"
SHORT_ID_CHARS = 4
ENV_PREFIX = "AOE_HDR_"
DEFAULT_HEADER = "X-AoE-Tags"
DEFAULT_SESSION_TAGGING = "short_id"
DEFAULT_TOOL = "unknown"
AOE_FALLBACK = "/usr/local/bin/aoe"
DEFAULT_TIMEOUT = 2.0

SESSION_TAGGING_MODES = ("none", "short_id", "full_id")
MATCH_KINDS_M0 = ("project", "manual")
MATCH_KINDS_M1 = ("group", "tool", "title_regex")

TAG_RE = re.compile(r"^[a-z0-9][a-z0-9._:-]*$")
HEADER_NAME_RE = re.compile(r"^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$")
ENV_NAME_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
FIELD_RE = re.compile(r"\{([a-z_]+)\}")

#: Hook environment → flag fallback. The hook line passes these explicitly; the
#: direct command form (``!aoe-llm-tags value``) relies on the variables AOE
#: already injects into the agent process.
ENV_FALLBACKS = {
    "session": "AOE_SESSION_ID",
    "profile": "AOE_PROFILE",
    "tool": "AOE_TOOL",
    "project": "AOE_PROJECT_PATH",
    "group": "AOE_GROUP_PATH",
    "title": "AOE_SESSION_TITLE",
}


class AoeError(RuntimeError):
    """A `aoe` call that could not be made, failed, or printed something else."""


@dataclass(frozen=True)
class Header:
    name: str
    template: str
    env_var: str
    enabled: bool = True


@dataclass(frozen=True)
class Rule:
    tag: str
    match_kind: str = "project"
    match_value: str = ""


DEFAULT_HEADERS: tuple[Header, ...] = (
    Header(
        name="X-AoE-Tags",
        template="aoe,feature:{feature},session:{session}",
        env_var="AOE_LLM_TAGS",
        enabled=True,
    ),
    Header(
        name="X-AoE-Session",
        template="{session_id}",
        env_var="AOE_HDR_X_AOE_SESSION",
        enabled=False,
    ),
)


@dataclass(frozen=True)
class Settings:
    """Validated settings. Malformed rows are dropped with a warning, never fatal."""

    headers: tuple[Header, ...] = DEFAULT_HEADERS
    rules: tuple[Rule, ...] = ()
    session_tagging: str = DEFAULT_SESSION_TAGGING
    feature_tag: str = ""
    warnings: tuple[str, ...] = ()

    @classmethod
    def from_table(
        cls,
        table: Mapping[str, Any] | None,
        warnings: Iterable[str] = (),
    ) -> "Settings":
        warns = list(warnings)
        table = table or {}
        headers = _parse_headers(table.get("headers"), warns)
        rules = _parse_rules(table.get("rules"), warns)

        tagging = table.get("session_tagging", DEFAULT_SESSION_TAGGING)
        if tagging not in SESSION_TAGGING_MODES:
            warns.append(
                f"settings: session_tagging = {tagging!r} is not one of "
                f"{', '.join(SESSION_TAGGING_MODES)}; using {DEFAULT_SESSION_TAGGING}"
            )
            tagging = DEFAULT_SESSION_TAGGING

        feature_tag = table.get("feature_tag", "")
        if not isinstance(feature_tag, str):
            warns.append("settings: feature_tag is not a string; ignoring it")
            feature_tag = ""
        elif feature_tag and not valid_tag(feature_tag):
            warns.append(
                f"settings: feature_tag {feature_tag!r} is not a valid tag "
                f"({TAG_RE.pattern}, ≤{TAG_MAX_CHARS} chars); ignoring it"
            )
            feature_tag = ""

        return cls(
            headers=headers,
            rules=rules,
            session_tagging=tagging,
            feature_tag=feature_tag,
            warnings=tuple(warns),
        )


@dataclass(frozen=True)
class Flags:
    """Per-field fallbacks; ``None`` means "not given", empty means "given empty"."""

    session: str = ""
    profile: str = ""
    tool: str = ""
    project: str = ""
    group: str = ""
    title: str = ""
    adapter: str = ""

    @classmethod
    def from_args(
        cls,
        args: Any = None,
        env: Mapping[str, str] | None = None,
    ) -> "Flags":
        env = _env(env)
        values: dict[str, str] = {}
        for name, variable in ENV_FALLBACKS.items():
            given = getattr(args, name, None) if args is not None else None
            values[name] = given if given else str(env.get(variable, ""))
        values["adapter"] = (getattr(args, "adapter", None) or "") if args is not None else ""
        return cls(**values)


@dataclass(frozen=True)
class SessionFacts:
    """What the session is, already resolved. Empty string = genuinely absent."""

    id: str = ""
    title: str = ""
    path: str = ""
    group: str = ""
    tool: str = DEFAULT_TOOL
    profile: str = ""
    branch: str = ""


@dataclass(frozen=True)
class ResolvedHeader:
    header: str
    env_var: str
    value: str


@dataclass(frozen=True)
class Snapshot:
    """One resolution pass: what the session is, what settings applied, what came out.

    Every verb uses this, so `hook`, `value`, `setup` and `doctor` cannot drift.
    """

    facts: SessionFacts
    settings: Settings
    resolved: tuple[ResolvedHeader, ...]
    warnings: tuple[str, ...]
    raw: Mapping[str, Any] = field(default_factory=dict)


Runner = Callable[[Sequence[str]], Any]


# --- aoe subprocess ---------------------------------------------------------


def aoe_bin(env: Mapping[str, str] | None = None) -> str:
    """``AOE_BIN`` → `which aoe` → the documented fallback (parameters §2)."""
    env = _env(env)
    return str(env.get("AOE_BIN") or shutil.which("aoe") or AOE_FALLBACK)


def aoe_json(
    args: Sequence[str],
    *,
    env: Mapping[str, str] | None = None,
    timeout: float = DEFAULT_TIMEOUT,
) -> Any:
    """Run one read-only ``aoe`` command and parse its JSON. Never blocks long."""
    bin_path = aoe_bin(env)
    try:
        proc = subprocess.run(
            [bin_path, *args],
            capture_output=True,
            text=True,
            timeout=timeout,
            stdin=subprocess.DEVNULL,
            check=False,
        )
    except subprocess.TimeoutExpired:
        raise AoeError(f"`{bin_path} {args[0] if args else ''}` timed out after {timeout:g}s")
    except OSError as exc:
        raise AoeError(f"`{bin_path}` could not run: {exc}")
    if proc.returncode != 0:
        detail = " ".join((proc.stderr or "").split())
        raise AoeError(
            f"`{bin_path} {args[0] if args else ''}` exited {proc.returncode}"
            + (f": {detail}" if detail else "")
        )
    try:
        return json.loads(proc.stdout)
    except ValueError as exc:
        raise AoeError(f"`{bin_path} {args[0] if args else ''}` printed invalid JSON: {exc}")


def default_runner(
    env: Mapping[str, str] | None = None,
    timeout: float = DEFAULT_TIMEOUT,
) -> Runner:
    return lambda args: aoe_json(args, env=env, timeout=timeout)


# --- session facts ----------------------------------------------------------


def resolve_session(
    flags: Flags | None = None,
    *,
    runner: Runner | None = None,
    env: Mapping[str, str] | None = None,
    timeout: float = DEFAULT_TIMEOUT,
) -> tuple[SessionFacts, tuple[str, ...]]:
    """Row → flags → defaults. A failed lookup is a warning, never an error."""
    flags = flags or Flags()
    runner = runner or default_runner(env, timeout)
    warnings: list[str] = []
    row: Mapping[str, Any] = {}

    if flags.session:
        argv = ["session", "show", flags.session]
        if flags.profile:
            argv += ["-p", flags.profile]
        argv.append("--json")
        try:
            loaded = runner(argv)
        except AoeError as exc:
            warnings.append(
                f"{exc}; falling back to the hook environment "
                "(group/title will be empty)"
            )
            loaded = None
        if isinstance(loaded, Mapping):
            row = loaded
        elif loaded is not None:
            warnings.append("`aoe session show` did not return a JSON object; using the hook environment")

    facts = SessionFacts(
        id=_first(row.get("id"), flags.session),
        title=_first(row.get("title"), flags.title),
        path=_first(row.get("path"), flags.project),
        group=_first(row.get("group"), flags.group),
        tool=_first(row.get("tool"), flags.tool, DEFAULT_TOOL),
        profile=_first(row.get("profile"), flags.profile),
    )
    return facts, tuple(warnings)


def resolve_branch(
    facts: SessionFacts,
    *,
    runner: Runner | None = None,
    env: Mapping[str, str] | None = None,
    timeout: float = DEFAULT_TIMEOUT,
) -> tuple[str, tuple[str, ...]]:
    """``{branch}`` needs ``aoe list --json``; ask only when a template wants it."""
    if not facts.id:
        return "", ()
    runner = runner or default_runner(env, timeout)
    try:
        rows = runner(["list", "--json"])
    except AoeError as exc:
        return "", (f"{exc}; {{branch}} renders empty",)
    if not isinstance(rows, Sequence) or isinstance(rows, (str, bytes)):
        return "", ("`aoe list --json` did not return a list; {branch} renders empty",)
    for row in rows:
        if not isinstance(row, Mapping) or row.get("id") != facts.id:
            continue
        worktree = row.get("worktree")
        if isinstance(worktree, Mapping):
            branch = worktree.get("branch")
            if isinstance(branch, str):
                return branch, ()
    return "", ()


# --- fields and rendering ---------------------------------------------------


def short_id(session_id: str, mode: str = DEFAULT_SESSION_TAGGING) -> str:
    """``short_id`` → ``S-`` + the first 4 characters; derivable from `aoe list`."""
    if not session_id:
        return ""
    if mode == "none":
        return ""
    if mode == "full_id":
        return session_id
    return f"{SHORT_ID_PREFIX}{session_id[:SHORT_ID_CHARS]}"


def slug(text: str) -> str:
    return re.sub(r"[^a-z0-9]+", "-", text.lower()).strip("-")


def project_slug(path: str) -> str:
    return slug(Path(path).name) if path else ""


def derive_env_var(header: str) -> str:
    """``X-AoE-Session`` → ``AOE_HDR_X_AOE_SESSION`` (parameters §3)."""
    return ENV_PREFIX + re.sub(r"[^A-Za-z0-9]+", "_", header).strip("_").upper()


def valid_tag(tag: str) -> bool:
    return bool(TAG_RE.match(tag)) and len(tag) <= TAG_MAX_CHARS


def valid_header_name(name: str) -> bool:
    return bool(HEADER_NAME_RE.match(name)) and len(name.encode("utf-8")) <= 64


def matched_tags(settings: Settings, facts: SessionFacts) -> list[str]:
    """Every matching rule contributes; dedupe, then cap at 5 (parameters §5)."""
    slugged = project_slug(facts.path)
    tags: list[str] = []
    for rule in settings.rules:
        if rule.tag in tags:
            continue
        if rule_matches(rule, facts, slugged):
            tags.append(rule.tag)
        if len(tags) == TAG_CAP:
            break
    return tags


def rule_matches(rule: Rule, facts: SessionFacts, slugged_project: str = "") -> bool:
    """M0 kinds only: ``project`` and ``manual`` (parameters §3).

    ``project`` compares against the project slug (basename) or the full path;
    ``manual`` is an operator-written unconditional tag, which is what M0 can
    honour before manual tags are assignable in the UI (M3-T06).
    """
    if rule.match_kind == "manual":
        return True
    if rule.match_kind == "project":
        value = rule.match_value
        if not value:
            return False
        return value == (slugged_project or project_slug(facts.path)) or value == facts.path
    return False


def build_fields(
    settings: Settings,
    facts: SessionFacts,
    *,
    tags: Sequence[str] = (),
    branch: str = "",
) -> dict[str, str]:
    tags = list(tags)
    return {
        "session": short_id(facts.id, settings.session_tagging),
        "session_id": facts.id,
        "feature": tags[0] if tags else settings.feature_tag,
        "tags": ",".join(tags),
        "tool": facts.tool or DEFAULT_TOOL,
        "profile": facts.profile,
        "group": facts.group,
        "group_slug": slug(facts.group),
        "project": facts.path,
        "project_slug": project_slug(facts.path),
        "branch": branch,
    }


def render_template(template: str, fields: Mapping[str, str]) -> tuple[str, list[str]]:
    """Substitute ``{field}``; unknown fields render empty and are reported."""
    missing: list[str] = []

    def substitute(match: re.Match[str]) -> str:
        name = match.group(1)
        if name in fields:
            return fields[name]
        missing.append(name)
        return ""

    return FIELD_RE.sub(substitute, template), missing


def sanitize_value(value: str) -> str:
    """One printable line: aoe parses stdout as KEY=VALUE and does not trim.

    Newlines would truncate the variable; other control characters have no
    business in a header value. Non-ASCII stays — §5 allows it when the operator
    writes it into a template.
    """
    return "".join(ch for ch in value if ch >= " " and ch != "\x7f").strip()


def clamp_bytes(value: str, limit: int = VALUE_MAX_BYTES) -> tuple[str, bool]:
    data = value.encode("utf-8")
    if len(data) <= limit:
        return value, False
    return data[:limit].decode("utf-8", "ignore"), True


def enabled_headers(settings: Settings) -> tuple[Header, ...]:
    """The headers this launch will emit or map: enabled, in settings order."""
    return tuple(header for header in settings.headers if header.enabled)


def snapshot(
    flags: Flags | None = None,
    *,
    env: Mapping[str, str] | None = None,
    runner: Runner | None = None,
) -> Snapshot:
    """The whole pipeline: row → flags → settings → render, on one launch budget.

    One 2 s budget covers every subprocess (the row lookup and the optional
    ``aoe list`` for ``{branch}``), so a slow or missing `aoe` delays a launch
    once, never twice.
    """
    flags = flags or Flags()
    started = time.monotonic()
    facts, session_warnings = resolve_session(flags, runner=runner, env=env)
    elapsed = time.monotonic() - started
    raw = settings_module.load(profile=facts.profile or flags.profile, env=env)
    configured = Settings.from_table(raw.table, warnings=session_warnings + raw.warnings)
    resolved, render_warnings = render_headers(
        configured,
        facts,
        runner=runner,
        env=env,
        timeout=max(0.0, DEFAULT_TIMEOUT - elapsed),
    )
    return Snapshot(
        facts=facts,
        settings=configured,
        resolved=resolved,
        warnings=configured.warnings + render_warnings,
        raw=dict(raw.table),
    )


def render_headers(
    settings: Settings,
    facts: SessionFacts,
    *,
    runner: Runner | None = None,
    env: Mapping[str, str] | None = None,
    timeout: float = DEFAULT_TIMEOUT,
) -> tuple[tuple[ResolvedHeader, ...], tuple[str, ...]]:
    """Render every enabled header. Invalid rows are skipped with a warning.

    ``timeout`` is what is left of the hook's launch budget, not a fresh 2 s: the
    optional ``aoe list`` call for ``{branch}`` must not push the hook past it.
    """
    warnings: list[str] = []
    enabled = enabled_headers(settings)

    branch = ""
    if facts.id and any("{branch}" in header.template for header in enabled):
        if timeout <= 0:
            warnings.append("session lookup used the whole 2s budget; {branch} renders empty")
        else:
            branch, branch_warnings = resolve_branch(facts, runner=runner, env=env, timeout=timeout)
            warnings.extend(branch_warnings)

    fields = build_fields(settings, facts, tags=matched_tags(settings, facts), branch=branch)

    resolved: list[ResolvedHeader] = []
    for header in enabled:
        if not valid_header_name(header.name):
            warnings.append(
                f"settings: {header.name!r} is not a valid HTTP header name; skipping it"
            )
            continue
        value, missing = render_template(header.template, fields)
        if missing:
            warnings.append(
                f"settings: template {header.template!r} for {header.name} uses unknown "
                f"field(s) {', '.join('{' + name + '}' for name in dict.fromkeys(missing))}; "
                "they render empty"
            )
        value = sanitize_value(value)
        value, truncated = clamp_bytes(value)
        if truncated:
            warnings.append(
                f"settings: {header.name} renders more than {VALUE_MAX_BYTES} bytes; truncated"
            )
        env_var = header.env_var or derive_env_var(header.name)
        if not ENV_NAME_RE.match(env_var):
            warnings.append(f"settings: {env_var!r} is not a valid environment name; skipping it")
            continue
        resolved.append(ResolvedHeader(header=header.name, env_var=env_var, value=value))

    return tuple(resolved), tuple(warnings)


def hook_lines(resolved: Iterable[ResolvedHeader]) -> list[str]:
    """The hook's stdout: exactly ``NAME=value`` per enabled header, nothing else."""
    return [f"{item.env_var}={item.value}" for item in resolved]


# --- settings normalisation -------------------------------------------------


def _parse_headers(rows: Any, warnings: list[str]) -> tuple[Header, ...]:
    if rows is None:
        return DEFAULT_HEADERS
    if not isinstance(rows, list):
        warnings.append("settings: headers is not an array of tables; using the built-in header set")
        return DEFAULT_HEADERS
    headers: list[Header] = []
    for index, row in enumerate(rows, start=1):
        if not isinstance(row, Mapping):
            warnings.append(f"settings: headers[{index}] is not a table; skipping it")
            continue
        name = row.get("header")
        if not isinstance(name, str) or not name:
            warnings.append(f"settings: headers[{index}] has no header name; skipping it")
            continue
        template = row.get("template", "")
        if not isinstance(template, str):
            warnings.append(f"settings: headers[{index}] ({name}) template is not a string; skipping it")
            continue
        enabled = row.get("enabled", True)
        if not isinstance(enabled, bool):
            warnings.append(
                f"settings: headers[{index}] ({name}) enabled is not a boolean; treating it as true"
            )
            enabled = True
        env_var = row.get("env_var") or derive_env_var(name)
        if not isinstance(env_var, str):
            warnings.append(f"settings: headers[{index}] ({name}) env_var is not a string; skipping it")
            continue
        headers.append(Header(name=name, template=template, env_var=env_var, enabled=enabled))
    return tuple(headers)


def _parse_rules(rows: Any, warnings: list[str]) -> tuple[Rule, ...]:
    if rows is None:
        return ()
    if not isinstance(rows, list):
        warnings.append("settings: rules is not an array of tables; ignoring it")
        return ()
    rules: list[Rule] = []
    for index, row in enumerate(rows, start=1):
        if not isinstance(row, Mapping):
            warnings.append(f"settings: rules[{index}] is not a table; skipping it")
            continue
        tag = row.get("tag")
        if not isinstance(tag, str) or not valid_tag(tag):
            warnings.append(
                f"settings: rules[{index}] tag {tag!r} is not a valid tag "
                f"({TAG_RE.pattern}, ≤{TAG_MAX_CHARS} chars); skipping it"
            )
            continue
        kind = row.get("match_kind", "project")
        if not isinstance(kind, str):
            warnings.append(f"settings: rules[{index}] ({tag}) match_kind is not a string; skipping it")
            continue
        if kind not in MATCH_KINDS_M0:
            if kind in MATCH_KINDS_M1:
                warnings.append(
                    f"settings: rules[{index}] ({tag}) match_kind {kind!r} is not evaluated "
                    "until M1; it never matches yet"
                )
            else:
                warnings.append(
                    f"settings: rules[{index}] ({tag}) match_kind {kind!r} is unknown; "
                    "it never matches"
                )
        value = row.get("match_value", "")
        if not isinstance(value, str):
            warnings.append(f"settings: rules[{index}] ({tag}) match_value is not a string; skipping it")
            continue
        rules.append(Rule(tag=tag, match_kind=kind, match_value=value))
    return tuple(rules)


# --- small helpers ----------------------------------------------------------


def _first(*values: Any) -> str:
    for value in values:
        if isinstance(value, str) and value:
            return value
    return ""


def _env(env: Mapping[str, str] | None) -> Mapping[str, str]:
    if env is not None:
        return env
    return os.environ
