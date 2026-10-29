"""CLI contract: stdout is only NAME=value, exit codes, and a hook never aborts a launch."""

from __future__ import annotations

import contextlib
import io
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from llm_tags import cli, resolver

from . import helpers

SESSION = {
    "id": "cf11af87da7e4bf0",
    "title": "Lithuanians",
    "path": "/var/lib/aoe/android-keyboard-worktrees/lithuanians",
    "group": "",
    "tool": "pi-proxy",
    "profile": "main",
}


class CliCase(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        self.env = helpers.app_env(
            self.tmp,
            AOE_SESSION_ID=SESSION["id"],
            AOE_PROFILE=SESSION["profile"],
            AOE_TOOL=SESSION["tool"],
        )
        helpers.write_config(self.env, helpers.settings_config('feature_tag = "review"'))

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def run_cli(self, *argv: str, runner=None):
        """Run the CLI with the ambient aoe replaced by a fake (no daemon, no network)."""
        runner = runner or helpers.FakeAoe(session=SESSION)
        stdout, stderr = io.StringIO(), io.StringIO()
        with mock.patch.object(resolver, "aoe_json", side_effect=lambda args, **_: runner(args)):
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                code = cli.main(list(argv), env=self.env)
        return code, stdout.getvalue(), stderr.getvalue()


class Hook(CliCase):
    def test_prints_only_name_value_lines(self) -> None:
        code, out, err = self.run_cli("hook")
        self.assertEqual(code, 0)
        self.assertEqual(out, "AOE_LLM_TAGS=aoe,feature:review,session:S-cf11\n")
        self.assertEqual(err, "")

    def test_every_enabled_header_gets_its_own_line(self) -> None:
        helpers.write_config(
            self.env,
            helpers.settings_config(
                'headers = [\n'
                '  { header = "X-AoE-Tags", template = "{tool}", env_var = "ONE" },\n'
                '  { header = "X-AoE-Session", template = "{session_id}", env_var = "TWO" },\n'
                "]\n"
            ),
        )
        code, out, _ = self.run_cli("hook")
        self.assertEqual(code, 0)
        self.assertEqual(out, "ONE=pi-proxy\nTWO=cf11af87da7e4bf0\n")

    def test_malformed_settings_warn_but_still_exit_zero(self) -> None:
        (Path(self.env["XDG_CONFIG_HOME"]) / "agent-of-empires" / "config.toml").write_text(
            "broken = = \n", encoding="utf-8"
        )
        code, out, err = self.run_cli("hook")
        self.assertEqual(code, 0)
        self.assertEqual(out, "AOE_LLM_TAGS=aoe,feature:,session:S-cf11\n")
        self.assertIn("malformed TOML", err)
        self.assertTrue(err.startswith("aoe-llm-tags: "))

    def test_aoe_failure_warns_but_still_exit_zero(self) -> None:
        code, out, err = self.run_cli("hook", runner=helpers.FakeAoe(error="`aoe` exited 1"))
        self.assertEqual(code, 0)
        self.assertEqual(out, "AOE_LLM_TAGS=aoe,feature:review,session:S-cf11\n")
        self.assertIn("falling back to the hook environment", err)

    def test_unexpected_error_still_exit_zero(self) -> None:
        with mock.patch.object(resolver, "snapshot", side_effect=RuntimeError("boom")):
            stdout, stderr = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                code = cli.main(["hook"], env=self.env)
        self.assertEqual(code, 0)
        self.assertEqual(stdout.getvalue(), "")
        self.assertIn("boom", stderr.getvalue())

    def test_warnings_are_not_printed_twice(self) -> None:
        _, _, err = self.run_cli("hook", runner=helpers.FakeAoe(error="same failure"))
        self.assertEqual(err.count("same failure"), 1)

    def test_budget_is_shared_between_row_and_branch(self) -> None:
        """One budget: the branch lookup gets what the row lookup left, not a new 2 s."""
        helpers.write_config(
            self.env,
            helpers.settings_config(
                'headers = [{ header = "X-Branch", template = "{branch}", env_var = "BR" }]\n'
            ),
        )
        seen: list[float] = []
        real = resolver.resolve_branch

        def spy(facts, **kwargs):
            seen.append(kwargs.get("timeout", -1))
            return real(facts, **kwargs)

        with mock.patch.object(resolver, "resolve_branch", side_effect=spy):
            self.run_cli("hook")
        self.assertEqual(len(seen), 1)
        self.assertLessEqual(seen[0], resolver.DEFAULT_TIMEOUT)


class Value(CliCase):
    def test_prints_one_value(self) -> None:
        code, out, _ = self.run_cli("value", "X-AoE-Tags")
        self.assertEqual(code, 0)
        self.assertEqual(out, "aoe,feature:review,session:S-cf11\n")

    def test_header_match_is_case_insensitive(self) -> None:
        _, out, _ = self.run_cli("value", "x-aoe-tags")
        self.assertEqual(out, "aoe,feature:review,session:S-cf11\n")

    def test_unknown_header_warns_and_exits_zero(self) -> None:
        code, out, err = self.run_cli("value", "X-Nope")
        self.assertEqual(code, 0)
        self.assertEqual(out, "")
        self.assertIn("not enabled", err)

    def test_default_header_is_the_first_enabled_one(self) -> None:
        _, out, _ = self.run_cli("value")
        self.assertEqual(out, "aoe,feature:review,session:S-cf11\n")


class Unimplemented(CliCase):
    def test_verify_reports_its_milestone(self) -> None:
        code, out, err = self.run_cli("verify")
        self.assertEqual(code, 2)
        self.assertEqual(out, "")
        self.assertIn("M2", err)

    def test_verify_ignores_extra_flags(self) -> None:
        code, _, err = self.run_cli("verify", "--session", "abc")
        self.assertEqual(code, 2)
        self.assertIn("M2", err)


class Usage(CliCase):
    def test_no_command_is_a_usage_error(self) -> None:
        with contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit) as caught:
                cli.main([], env=self.env)
        self.assertEqual(caught.exception.code, 2)

    def test_unknown_command_is_a_usage_error(self) -> None:
        with contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit) as caught:
                cli.main(["nope"], env=self.env)
        self.assertEqual(caught.exception.code, 2)


if __name__ == "__main__":  # pragma: no cover
    unittest.main()
