"""M0-T03: the resolver's rules, tags, fields and value rules (no aoe, no network)."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from llm_tags import resolver
from llm_tags.resolver import (
    DEFAULT_HEADER,
    Header,
    Rule,
    SessionFacts,
    Settings,
    clamp_bytes,
    derive_env_var,
    enabled_headers,
    matched_tags,
    render_headers,
    render_template,
    sanitize_value,
    short_id,
    snapshot,
    valid_header_name,
    valid_tag,
)

from . import helpers

FACTS = SessionFacts(
    id="cf11af87da7e4bf0",
    title="Lithuanians",
    path="/var/lib/aoe/android-keyboard-worktrees/lithuanians",
    group="Backend/web",
    tool="pi-proxy",
    profile="main",
)


class TagRules(unittest.TestCase):
    def test_rules_contribute_in_order(self) -> None:
        settings = Settings(
            rules=(
                Rule(tag="alpha", match_kind="manual"),
                Rule(tag="review", match_kind="project", match_value="lithuanians"),
                Rule(tag="beta", match_kind="manual"),
            )
        )
        self.assertEqual(matched_tags(settings, FACTS), ["alpha", "review", "beta"])

    def test_duplicate_tags_appear_once(self) -> None:
        settings = Settings(
            rules=(Rule(tag="alpha", match_kind="manual"), Rule(tag="alpha", match_kind="manual"))
        )
        self.assertEqual(matched_tags(settings, FACTS), ["alpha"])

    def test_tags_are_capped_at_five(self) -> None:
        rules = tuple(Rule(tag=f"tag{i}", match_kind="manual") for i in range(7))
        self.assertEqual(
            matched_tags(Settings(rules=rules), FACTS),
            ["tag0", "tag1", "tag2", "tag3", "tag4"],
        )
        self.assertEqual(resolver.TAG_CAP, 5)

    def test_project_rule_matches_slug_or_full_path(self) -> None:
        by_slug = Settings(rules=(Rule(tag="x", match_kind="project", match_value="lithuanians"),))
        by_path = Settings(rules=(Rule(tag="y", match_kind="project", match_value=FACTS.path),))
        other = Settings(rules=(Rule(tag="z", match_kind="project", match_value="other"),))
        self.assertEqual(matched_tags(by_slug, FACTS), ["x"])
        self.assertEqual(matched_tags(by_path, FACTS), ["y"])
        self.assertEqual(matched_tags(other, FACTS), [])

    def test_manual_rule_matches_every_session(self) -> None:
        settings = Settings(rules=(Rule(tag="always", match_kind="manual"),))
        self.assertEqual(matched_tags(settings, SessionFacts()), ["always"])

    def test_m1_rule_kinds_never_match_and_say_so(self) -> None:
        settings = Settings.from_table(
            {"rules": [{"tag": "grp", "match_kind": "group", "match_value": "Backend"}]}
        )
        self.assertEqual(matched_tags(settings, FACTS), [])
        self.assertTrue(any("not evaluated until M1" in warning for warning in settings.warnings))

    def test_invalid_rules_are_dropped_with_a_warning(self) -> None:
        settings = Settings.from_table(
            {
                "rules": [
                    {"tag": "Bad Tag"},
                    {"tag": ""},
                    {"tag": "x" * 33},
                    {"tag": "good", "match_kind": "project", "match_value": "lithuanians"},
                    "not-a-table",
                ]
            }
        )
        self.assertEqual([rule.tag for rule in settings.rules], ["good"])
        self.assertEqual(len([w for w in settings.warnings if "not a valid tag" in w]), 3)

    def test_tag_grammar(self) -> None:
        for good in ("a", "review", "backend-web", "a.b_c:d", "x1"):
            self.assertTrue(valid_tag(good), good)
        for bad in ("", "-x", "Review", "has space", "x" * 33, "emoji✈"):
            self.assertFalse(valid_tag(bad), bad)


class SessionShortId(unittest.TestCase):
    def test_modes(self) -> None:
        self.assertEqual(short_id("cf11af87da7e4bf0"), "S-cf11")
        self.assertEqual(short_id("cf11af87da7e4bf0", "short_id"), "S-cf11")
        self.assertEqual(short_id("cf11af87da7e4bf0", "full_id"), "cf11af87da7e4bf0")
        self.assertEqual(short_id("cf11af87da7e4bf0", "none"), "")
        self.assertEqual(short_id(""), "")

    def test_unknown_mode_falls_back_to_short_id_with_a_warning(self) -> None:
        settings = Settings.from_table({"session_tagging": "shouty"})
        self.assertEqual(settings.session_tagging, "short_id")
        self.assertTrue(any("session_tagging" in warning for warning in settings.warnings))


class Fields(unittest.TestCase):
    def test_every_field_renders(self) -> None:
        value, missing = render_template(
            "{session}|{session_id}|{feature}|{tags}|{tool}|{profile}|"
            "{group}|{group_slug}|{project}|{project_slug}|{branch}",
            resolver.build_fields(
                Settings(session_tagging="short_id"),
                FACTS,
                tags=["review"],
                branch="lithuanians",
            ),
        )
        self.assertEqual(missing, [])
        self.assertEqual(
            value,
            "S-cf11|cf11af87da7e4bf0|review|review|pi-proxy|main|"
            "Backend/web|backend-web|"
            "/var/lib/aoe/android-keyboard-worktrees/lithuanians|lithuanians|lithuanians",
        )

    def test_feature_falls_back_to_the_setting(self) -> None:
        fields = resolver.build_fields(Settings(feature_tag="ops"), FACTS, tags=[])
        self.assertEqual(fields["feature"], "ops")
        self.assertEqual(fields["tags"], "")

    def test_unknown_field_renders_empty_and_is_reported(self) -> None:
        value, missing = render_template("{tool}-{nope}", {"tool": "pi"})
        self.assertEqual(value, "pi-")
        self.assertEqual(missing, ["nope"])

    def test_group_slug_slugs_the_whole_group(self) -> None:
        self.assertEqual(resolver.slug("Backend/web"), "backend-web")
        self.assertEqual(resolver.slug("  "), "")
        self.assertEqual(resolver.project_slug(""), "")


class EnvNames(unittest.TestCase):
    def test_default_variable(self) -> None:
        self.assertEqual(derive_env_var("X-AoE-Tags"), "AOE_HDR_X_AOE_TAGS")

    def test_punctuation_becomes_underscore(self) -> None:
        self.assertEqual(derive_env_var("X.Weird~Header"), "AOE_HDR_X_WEIRD_HEADER")

    def test_pinned_variable_wins(self) -> None:
        resolved, warnings = render_headers(
            Settings(headers=(Header("X-Custom", "v", "MY_VAR"),)), SessionFacts()
        )
        self.assertEqual([item.env_var for item in resolved], ["MY_VAR"])
        self.assertEqual(warnings, ())

    def test_invalid_env_name_skips_that_header(self) -> None:
        resolved, warnings = render_headers(
            Settings(headers=(Header("X-Custom", "v", "9bad name"),)), SessionFacts()
        )
        self.assertEqual(resolved, ())
        self.assertTrue(any("not a valid environment name" in warning for warning in warnings))


class ValueRules(unittest.TestCase):
    def test_control_characters_are_stripped_and_outer_space_trimmed(self) -> None:
        self.assertEqual(sanitize_value("  aoe,\tfeature:x\nsession:S-1\r\n"), "aoe,feature:xsession:S-1")

    def test_non_ascii_is_kept(self) -> None:
        self.assertEqual(sanitize_value("feature:café"), "feature:café")

    def test_over_long_values_are_clamped_to_octets(self) -> None:
        value, truncated = clamp_bytes("é" * 500)
        self.assertTrue(truncated)
        self.assertLessEqual(len(value.encode("utf-8")), resolver.VALUE_MAX_BYTES)
        self.assertEqual(len(value.encode("utf-8")), 800)

    def test_short_values_are_untouched(self) -> None:
        self.assertEqual(clamp_bytes("abc"), ("abc", False))

    def test_over_long_render_warns(self) -> None:
        resolved, warnings = render_headers(
            Settings(headers=(Header("X-Long", "{project}", "X_LONG"),)),
            SessionFacts(path="/" + "a" * 900),
        )
        self.assertEqual(len(resolved), 1)
        self.assertTrue(any("truncated" in warning for warning in warnings))

    def test_header_name_grammar(self) -> None:
        self.assertTrue(valid_header_name("X-AoE-Tags"))
        self.assertFalse(valid_header_name("X AoE"))
        self.assertFalse(valid_header_name(""))


class Rendering(unittest.TestCase):
    def test_one_line_per_enabled_header(self) -> None:
        resolved, _ = render_headers(Settings(), FACTS)
        self.assertEqual([item.header for item in resolved], [DEFAULT_HEADER])
        self.assertEqual(
            resolver.hook_lines(resolved), ["AOE_LLM_TAGS=aoe,feature:,session:S-cf11"]
        )

    def test_disabled_header_is_not_emitted(self) -> None:
        settings = Settings(
            headers=(
                Header("X-AoE-Tags", "{tool}", "A"),
                Header("X-AoE-Session", "{session_id}", "B", enabled=False),
            )
        )
        self.assertEqual([h.name for h in enabled_headers(settings)], ["X-AoE-Tags"])
        resolved, _ = render_headers(settings, FACTS)
        self.assertEqual(resolver.hook_lines(resolved), ["A=pi-proxy"])

    def test_no_headers_prints_nothing(self) -> None:
        resolved, _ = render_headers(Settings(headers=()), FACTS)
        self.assertEqual(resolver.hook_lines(resolved), [])

    def test_invalid_header_name_skips_only_that_header(self) -> None:
        settings = Settings(
            headers=(
                Header("X Bad", "v", "A"),
                Header("X-Good", "ok", "B"),
            )
        )
        resolved, warnings = render_headers(settings, FACTS)
        self.assertEqual(resolver.hook_lines(resolved), ["B=ok"])
        self.assertTrue(any("not a valid HTTP header name" in warning for warning in warnings))

    def test_empty_value_is_still_emitted(self) -> None:
        resolved, _ = render_headers(Settings(headers=(Header("X-Empty", "", "EMPTY"),)), FACTS)
        self.assertEqual(resolver.hook_lines(resolved), ["EMPTY="])

    def test_headers_keep_settings_order(self) -> None:
        settings = Settings(
            headers=(Header("X-Two", "2", "TWO"), Header("X-One", "1", "ONE"))
        )
        resolved, _ = render_headers(settings, FACTS)
        self.assertEqual(resolver.hook_lines(resolved), ["TWO=2", "ONE=1"])


class BranchLookup(unittest.TestCase):
    def test_branch_is_not_requested_unless_a_template_needs_it(self) -> None:
        runner = helpers.FakeAoe(session={"id": FACTS.id})
        render_headers(Settings(), FACTS, runner=runner, env={})
        self.assertEqual(runner.calls, [])

    def test_branch_costs_one_list_call(self) -> None:
        runner = helpers.FakeAoe(
            session={"id": FACTS.id}, rows=[{"id": FACTS.id, "worktree": {"branch": "main"}}]
        )
        resolved, warnings = render_headers(
            Settings(headers=(Header("X-Branch", "{branch}", "BRANCH"),)),
            FACTS,
            runner=runner,
            env={},
        )
        self.assertEqual(resolver.hook_lines(resolved), ["BRANCH=main"])
        self.assertEqual(warnings, ())
        self.assertEqual(runner.subcommands, ["list"])

    def test_exhausted_budget_renders_empty_without_calling_aoe(self) -> None:
        runner = helpers.FakeAoe()
        resolved, warnings = render_headers(
            Settings(headers=(Header("X-Branch", "{branch}", "BRANCH"),)),
            FACTS,
            runner=runner,
            timeout=0,
        )
        self.assertEqual(resolver.hook_lines(resolved), ["BRANCH="])
        self.assertTrue(any("budget" in warning for warning in warnings))
        self.assertEqual(runner.calls, [])

    def test_missing_branch_is_empty_not_fatal(self) -> None:
        runner = helpers.FakeAoe(session={"id": FACTS.id}, rows=[])
        resolved, _ = render_headers(
            Settings(headers=(Header("X-Branch", "{branch}", "BRANCH"),)),
            FACTS,
            runner=runner,
        )
        self.assertEqual(resolver.hook_lines(resolved), ["BRANCH="])


class SnapshotResolution(unittest.TestCase):
    def _env(self, tmp: Path, settings: str = "", profile: str = "") -> dict[str, str]:
        env = helpers.app_env(tmp)
        helpers.write_config(env, helpers.settings_config(settings), profile=profile)
        return env

    def test_row_wins_over_flags(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = self._env(Path(tmp))
            runner = helpers.FakeAoe(
                session={
                    "id": "row00001",
                    "title": "From row",
                    "path": "/p/row",
                    "group": "G",
                    "tool": "pi-proxy",
                    "profile": "main",
                }
            )
            snap = snapshot(
                resolver.Flags(session="flag0001", title="From flag", project="/p/flag"),
                env=env,
                runner=runner,
            )
            self.assertEqual(snap.facts.id, "row00001")
            self.assertEqual(snap.facts.title, "From row")
            self.assertEqual(snap.facts.path, "/p/row")
            self.assertEqual(snap.resolved[0].value, "aoe,feature:,session:S-row0")

    def test_flags_are_used_when_the_row_cannot_be_read(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = self._env(Path(tmp))
            runner = helpers.FakeAoe(error="`aoe session show` exited 1")
            snap = snapshot(
                resolver.Flags(session="flag0001", project="/p/flag", tool="pi-proxy"), env=env, runner=runner
            )
            self.assertEqual(snap.facts.id, "flag0001")
            self.assertEqual(snap.facts.path, "/p/flag")
            self.assertEqual(snap.facts.tool, "pi-proxy")
            self.assertTrue(
                any("falling back to the hook environment" in w for w in snap.warnings)
            )
            self.assertEqual(snap.resolved[0].value, "aoe,feature:,session:S-flag")

    def test_no_session_at_all_degrades_to_defaults(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = self._env(Path(tmp))
            snap = snapshot(resolver.Flags(), env=env, runner=helpers.FakeAoe())
            self.assertEqual(snap.facts.tool, "unknown")
            self.assertEqual(snap.resolved[0].value, "aoe,feature:,session:")

    def test_settings_come_from_the_profile_override(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = self._env(Path(tmp), settings='feature_tag = "global"')
            helpers.write_config(env, helpers.settings_config('feature_tag = "profile"'), profile="main")
            snap = snapshot(
                resolver.Flags(profile="main", tool="pi"), env=env, runner=helpers.FakeAoe()
            )
            self.assertEqual(snap.resolved[0].value, "aoe,feature:profile,session:")

    def test_broken_settings_still_render_the_default_header(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = helpers.app_env(Path(tmp))
            (Path(env["XDG_CONFIG_HOME"]) / "agent-of-empires").mkdir(parents=True)
            (Path(env["XDG_CONFIG_HOME"]) / "agent-of-empires" / "config.toml").write_text(
                "this is not toml\n", encoding="utf-8"
            )
            snap = snapshot(resolver.Flags(tool="pi"), env=env, runner=helpers.FakeAoe())
            self.assertEqual([item.header for item in snap.resolved], [DEFAULT_HEADER])
            self.assertTrue(any("malformed TOML" in w for w in snap.warnings))

    def test_raw_table_is_available_to_doctor(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = self._env(Path(tmp), settings='last_verified = "2026-10-29"')
            snap = snapshot(resolver.Flags(tool="pi"), env=env, runner=helpers.FakeAoe())
            self.assertEqual(snap.raw["last_verified"], "2026-10-29")


if __name__ == "__main__":  # pragma: no cover
    unittest.main()
