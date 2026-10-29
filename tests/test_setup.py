"""M0-T05: ``setup --print`` emits exactly the pasteable lines, and nothing else."""

from __future__ import annotations

import contextlib
import io
import json
import shlex
import tempfile
import tomllib
import unittest
from pathlib import Path
from unittest import mock

from llm_tags import cli, resolver, setup

from . import helpers

SESSION = {"id": "cf11af87da7e4bf0", "tool": "pi-proxy", "profile": "main"}

TWO_HEADERS = (
    'headers = [\n'
    '  { header = "X-AoE-Tags", template = "a", env_var = "AOE_LLM_TAGS" },\n'
    '  { header = "X-AoE-Session", template = "{session_id}", env_var = "AOE_HDR_X_AOE_SESSION" },\n'
    "]\n"
)


class SetupCase(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        self.env = helpers.app_env(self.tmp, AOE_BIN=str(self.tmp / "bin" / "aoe"))
        helpers.write_executable(self.tmp / "bin" / "aoe", "#!/bin/sh\nexit 0\n")
        self.env["AOE_SESSION_ID"] = SESSION["id"]
        self.env["AOE_TOOL"] = SESSION["tool"]
        self.env["AOE_PROFILE"] = SESSION["profile"]
        helpers.write_config(self.env, helpers.settings_config(""))
        self.runner = helpers.FakeAoe(session=SESSION)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def run_cli(self, *argv: str):
        stdout, stderr = io.StringIO(), io.StringIO()
        with mock.patch.object(
            resolver, "aoe_json", side_effect=lambda args, **_: self.runner(args)
        ):
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                code = cli.main(list(argv), env=self.env)
        return code, stdout.getvalue(), stderr.getvalue()


class PrintedLines(SetupCase):
    def test_exactly_two_hook_lines_plus_one_mapping_line(self) -> None:
        code, out, err = self.run_cli("setup", "--print")
        self.assertEqual(code, 0, err)
        lines = out.splitlines()
        self.assertEqual(len(lines), 3, out)
        self.assertTrue(lines[0].startswith("before_session = '"))
        self.assertTrue(lines[1].startswith("before_start = '"))
        self.assertIn('headers["X-AoE-Tags"] = "$AOE_LLM_TAGS"', lines[2])

    def test_both_hook_keys_get_the_same_command(self) -> None:
        plan = setup.build(resolver.Flags.from_args(None, self.env), env=self.env)
        self.assertEqual(plan.hooks["before_session"], plan.hooks["before_start"])
        self.assertIn(f'AOE_BIN="{plan.aoe}"', plan.hooks["before_session"])
        self.assertIn(str(setup.cli_path()), plan.hooks["before_session"])

    def test_hook_line_is_valid_toml_and_the_argv_is_right(self) -> None:
        _, out, _ = self.run_cli("setup", "--print")
        parsed = tomllib.loads(out.splitlines()[0])
        self.assertEqual(list(parsed), ["before_session"])
        argv = shlex.split(parsed["before_session"])
        self.assertTrue(argv[0].startswith("AOE_BIN="))
        self.assertEqual(argv[0].split("=", 1)[1], str(self.tmp / "bin" / "aoe"))
        self.assertEqual(Path(argv[1]).name, "aoe-llm-tags")
        self.assertEqual(argv[2], "hook")
        for flag, variable in (
            ("--session", "AOE_SESSION_ID"),
            ("--profile", "AOE_PROFILE"),
            ("--tool", "AOE_TOOL"),
            ("--project", "AOE_PROJECT_PATH"),
            ("--group", "AOE_GROUP_PATH"),
            ("--title", "AOE_SESSION_TITLE"),
        ):
            self.assertIn(flag, argv)
            self.assertIn(f"${variable}", argv)

    def test_one_mapping_line_per_enabled_header(self) -> None:
        helpers.write_config(self.env, helpers.settings_config(TWO_HEADERS))
        code, out, _ = self.run_cli("setup", "--print")
        self.assertEqual(code, 0)
        lines = out.splitlines()
        self.assertEqual(len(lines), 4)
        self.assertIn('"X-AoE-Tags"', lines[2])
        self.assertIn('"$AOE_LLM_TAGS"', lines[2])
        self.assertIn('"X-AoE-Session"', lines[3])
        self.assertIn('"$AOE_HDR_X_AOE_SESSION"', lines[3])

    def test_disabled_header_is_not_mapped(self) -> None:
        helpers.write_config(
            self.env,
            helpers.settings_config(
                'headers = [{ header = "X-Off", template = "x", env_var = "OFF", enabled = false }]\n'
            ),
        )
        code, out, _ = self.run_cli("setup", "--print")
        self.assertEqual(code, 0)
        self.assertEqual(len(out.splitlines()), 2)  # only the hook lines

    def test_provider_id_comes_from_the_existing_config(self) -> None:
        helpers.pi_models(
            Path(self.env["HOME"]) / ".pi" / "agent" / "models.json",
            provider="litellm",
            headers={},
        )
        _, out, _ = self.run_cli("setup", "--print")
        self.assertIn("providers.litellm.headers", out)

    def test_placeholder_when_no_config_exists(self) -> None:
        _, out, _ = self.run_cli("setup", "--print")
        self.assertIn("providers.<provider-id>.headers", out)


class Failures(SetupCase):
    def test_unknown_adapter_names_the_known_ones(self) -> None:
        code, out, err = self.run_cli("setup", "--print", "--adapter", "nope")
        self.assertEqual(code, 1)
        self.assertEqual(out, "")
        self.assertIn("no adapter for tool", err)
        self.assertIn("pi", err)

    def test_unresearched_adapter_is_refused(self) -> None:
        code, _, err = self.run_cli("setup", "--print", "--adapter", "gemini")
        self.assertEqual(code, 1)
        self.assertIn("needs_research", err)

    def test_missing_aoe_binary_fails_loudly(self) -> None:
        self.env["AOE_BIN"] = str(self.tmp / "nope" / "aoe")
        code, _, err = self.run_cli("setup", "--print")
        self.assertEqual(code, 1)
        self.assertIn("AOE_BIN", err)

    def test_without_print_is_a_usage_error(self) -> None:
        code, out, err = self.run_cli("setup")
        self.assertEqual(code, 2)
        self.assertEqual(out, "")
        self.assertIn("--print", err)


class Json(SetupCase):
    def test_shape(self) -> None:
        code, out, _ = self.run_cli("setup", "--print", "--json")
        self.assertEqual(code, 0)
        plan = json.loads(out)
        self.assertEqual(plan["adapter"], "pi")
        self.assertEqual(set(plan["hooks"]), {"before_session", "before_start"})
        self.assertEqual(len(plan["mapping"]), 1)
        self.assertEqual(plan["mapping"][0]["header"], "X-AoE-Tags")
        self.assertTrue(Path(plan["cli"]).is_absolute())
        self.assertTrue(Path(plan["mapping"][0]["file"]).is_absolute())


class AdapterData(unittest.TestCase):
    def test_every_row_parses(self) -> None:
        from llm_tags import adapters

        rows, warnings = adapters.load()
        self.assertEqual(warnings, ())
        self.assertGreaterEqual(len(rows), 10)
        self.assertTrue(all(row.agent for row in rows))

    def test_tool_prefixes_and_names_resolve(self) -> None:
        from llm_tags import adapters

        rows, _ = adapters.load()
        self.assertEqual(adapters.match_tool(rows, "pi-proxy").agent, "pi")
        self.assertEqual(adapters.match_tool(rows, "pi").agent, "pi")
        self.assertEqual(adapters.find(rows, "claude").agent, "claude")
        self.assertIsNone(adapters.match_tool(rows, "totally-unknown"))
        self.assertIsNone(adapters.find(rows, ""))

    def test_unset_variable_makes_a_path_unresolvable(self) -> None:
        from llm_tags import adapters

        self.assertIsNone(adapters.expand_path("$NOPE/models.json", {}))
        self.assertEqual(
            adapters.expand_path("~/.pi/agent/models.json", {"HOME": "/h"}),
            Path("/h/.pi/agent/models.json"),
        )
        self.assertEqual(
            adapters.expand_path("$CFG/x.json", {"CFG": "/c"}), Path("/c/x.json")
        )


if __name__ == "__main__":  # pragma: no cover
    unittest.main()
