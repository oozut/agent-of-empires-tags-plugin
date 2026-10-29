"""M0-T04/M0-T11: doctor names each failure, and exits 0 only when nothing is broken."""

from __future__ import annotations

import contextlib
import io
import json
import tempfile
import unittest
from pathlib import Path

from llm_tags import cli, doctor, resolver, setup

from . import helpers

SESSION = {
    "id": "cf11af87da7e4bf0",
    "title": "Lithuanians",
    "path": "/var/lib/aoe/android-keyboard-worktrees/lithuanians",
    "group": "",
    "tool": "pi-proxy",
    "profile": "main",
}
HEADERS = {"X-AoE-Tags": "$AOE_LLM_TAGS"}


class DoctorCase(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        self.env = helpers.app_env(self.tmp, AOE_TOOL="pi-proxy", AOE_PROFILE="main")
        self.env["AOE_SESSION_ID"] = SESSION["id"]
        self.cli = helpers.write_executable(
            self.tmp / "plugin" / "bin" / "aoe-llm-tags", "#!/bin/sh\nexit 0\n"
        )
        self.aoe = helpers.fake_aoe_script(self.tmp / "bin" / "aoe", SESSION)
        self.models_path = Path(self.env["HOME"]) / ".pi" / "agent" / "models.json"
        self.wire()

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def wire(
        self,
        *,
        before_session: bool = True,
        before_start: bool = True,
        foreign_hook: bool = False,
        cli_exists: bool = True,
        aoe_exists: bool = True,
        headers: dict[str, str] | None = None,
        models_file: bool = True,
        config_file: bool = True,
        last_verified: str = "",
        settings: str = "",
    ) -> None:
        self.env["AOE_BIN"] = str(self.aoe if aoe_exists else self.tmp / "gone" / "aoe")
        cli = self.cli if cli_exists else self.tmp / "gone" / "bin" / "aoe-llm-tags"
        command = "/usr/local/bin/someone-else hook" if foreign_hook else setup.hook_command(
            cli, str(self.aoe)
        )
        hooks = ""
        if before_session:
            hooks += f"before_session = ['{command}']\n"
        if before_start:
            hooks += f"before_start = ['{command}']\n"

        config_path = Path(self.env["XDG_CONFIG_HOME"]) / "agent-of-empires" / "config.toml"
        if config_file:
            helpers.write_config(
                self.env,
                helpers.settings_config(
                    settings
                    or (f'last_verified = "{last_verified}"\n' if last_verified else ""),
                    hooks=hooks,
                ),
            )
        elif config_path.exists():
            config_path.unlink()

        if models_file:
            helpers.pi_models(self.models_path, headers=HEADERS if headers is None else headers)
        elif self.models_path.exists():
            self.models_path.unlink()

    def report(self, **flags: str) -> doctor.Report:
        parsed = resolver.Flags.from_args(None, self.env)
        if flags:
            parsed = resolver.Flags(**{**parsed.__dict__, **flags})
        return doctor.run(parsed, env=self.env)

    def run_cli(self, *argv: str):
        stdout, stderr = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            code = cli.main(list(argv), env=self.env)
        return code, stdout.getvalue(), stderr.getvalue()

    @staticmethod
    def check(report: doctor.Report, name: str) -> doctor.Check:
        for item in report.checks:
            if item.name == name:
                return item
        raise AssertionError(f"no check named {name!r} in {[c.name for c in report.checks]}")


class Wired(DoctorCase):
    def test_clean_install_passes(self) -> None:
        report = self.report()
        failed = [check for check in report.checks if check.status == doctor.FAIL]
        self.assertEqual(failed, [], doctor.format_text(report))
        self.assertTrue(report.ok)

    def test_cli_exits_zero_when_wired(self) -> None:
        code, out, err = self.run_cli("doctor")
        self.assertEqual(code, 0, err)
        self.assertIn("[ok  ]", out)
        self.assertIn("before_session hook", out)

    def test_every_check_is_present(self) -> None:
        names = {check.name for check in self.report().checks}
        for expected in (
            "effective config",
            "before_session hook",
            "before_start hook",
            "command path",
            "aoe binary",
            "harness adapter",
            "session row",
            "mapping X-AoE-Tags",
            "value rendering",
            "last verification",
        ):
            self.assertIn(expected, names)

    def test_second_header_gets_its_own_mapping_check(self) -> None:
        self.wire(
            settings='headers = [\n'
            '  { header = "X-AoE-Tags", template = "a", env_var = "A" },\n'
            '  { header = "X-AoE-Session", template = "{session_id}", env_var = "B" },\n'
            "]\n",
            headers={**HEADERS, "X-AoE-Session": "$AOE_HDR_X_AOE_SESSION"},
        )
        helpers.pi_models(
            self.models_path, headers={**HEADERS, "X-AoE-Session": "$AOE_HDR_X_AOE_SESSION"}
        )
        report = self.report()
        self.assertTrue(report.ok)
        self.assertEqual(self.check(report, "mapping X-AoE-Session").status, doctor.OK)

    def test_last_verification_is_recorded_when_present(self) -> None:
        self.wire(last_verified="2026-10-29 pi sink")
        self.assertEqual(self.check(self.report(), "last verification").status, doctor.OK)

    def test_no_session_skips_the_row_check_without_failing(self) -> None:
        self.env.pop("AOE_SESSION_ID")
        report = self.report(session="")
        self.assertEqual(self.check(report, "session row").status, doctor.SKIP)
        self.assertTrue(report.ok)

    def test_paths_are_absolute_and_printed(self) -> None:
        report = self.report()
        for label in ("app dir", "config", "plugin CLI", "aoe binary", "harness config"):
            self.assertTrue(Path(report.paths[label]).is_absolute(), label)
        text = doctor.format_text(report)
        for value in report.paths.values():
            self.assertIn(value, text)


class Breaks(DoctorCase):
    def test_missing_before_session(self) -> None:
        self.wire(before_session=False)
        check = self.check(self.report(), "before_session hook")
        self.assertEqual(check.status, doctor.FAIL)
        self.assertIn("host sessions will not send headers", check.detail)

    def test_missing_before_start(self) -> None:
        self.wire(before_start=False)
        check = self.check(self.report(), "before_start hook")
        self.assertEqual(check.status, doctor.FAIL)
        self.assertIn("sandboxed sessions will not send headers", check.detail)

    def test_foreign_hook_does_not_count(self) -> None:
        self.wire(foreign_hook=True)
        for name in ("before_session hook", "before_start hook"):
            check = self.check(self.report(), name)
            self.assertEqual(check.status, doctor.FAIL)
            self.assertIn("none runs aoe-llm-tags", check.detail)

    def test_plugin_moved(self) -> None:
        self.wire(cli_exists=False)
        check = self.check(self.report(), "command path")
        self.assertEqual(check.status, doctor.FAIL)
        self.assertIn("moved", check.detail)
        self.assertIn("gone", check.detail)

    def test_aoe_binary_unresolvable(self) -> None:
        self.wire(aoe_exists=False)
        check = self.check(self.report(), "aoe binary")
        self.assertEqual(check.status, doctor.FAIL)
        self.assertIn("AOE_BIN", check.detail)

    def test_header_not_mapped(self) -> None:
        self.wire(headers={})
        check = self.check(self.report(), "mapping X-AoE-Tags")
        self.assertEqual(check.status, doctor.FAIL)
        self.assertIn("is not mapped", check.detail)
        self.assertIn("litellm", check.detail)

    def test_harness_config_absent(self) -> None:
        self.wire(models_file=False)
        check = self.check(self.report(), "mapping X-AoE-Tags")
        self.assertEqual(check.status, doctor.FAIL)
        self.assertIn("does not exist", check.detail)

    def test_no_config_at_all(self) -> None:
        self.wire(config_file=False)
        check = self.check(self.report(), "effective config")
        self.assertEqual(check.status, doctor.FAIL)
        self.assertIn("no config file", check.detail)

    def test_unparsable_config(self) -> None:
        path = Path(self.env["XDG_CONFIG_HOME"]) / "agent-of-empires" / "config.toml"
        path.write_text("nope = = nope\n", encoding="utf-8")
        check = self.check(self.report(), "effective config")
        self.assertEqual(check.status, doctor.FAIL)
        self.assertIn("could not be parsed", check.detail)

    def test_unreadable_session_row(self) -> None:
        broken = helpers.write_executable(
            self.tmp / "bin" / "broken-aoe", "#!/bin/sh\necho 'daemon down' >&2\nexit 1\n"
        )
        self.env["AOE_BIN"] = str(broken)
        check = self.check(self.report(), "session row")
        self.assertEqual(check.status, doctor.FAIL)
        self.assertIn("falling back to the hook environment", check.detail)

    def test_unknown_adapter(self) -> None:
        check = self.check(self.report(adapter="nope"), "harness adapter")
        self.assertEqual(check.status, doctor.FAIL)
        self.assertIn("pass --adapter", check.detail)

    def test_broken_install_exits_one(self) -> None:
        self.wire(before_start=False)
        code, out, err = self.run_cli("doctor")
        self.assertEqual(code, 1)
        self.assertIn("before_start hook", err)
        self.assertIn("[fail]", out)

    def test_no_tool_outside_a_session_is_a_warning(self) -> None:
        self.aoe = helpers.fake_aoe_script(self.tmp / "bin" / "aoe-notool", {"id": SESSION["id"]})
        self.wire()
        self.env.pop("AOE_TOOL")
        report = self.report()
        check = self.check(report, "harness adapter")
        self.assertEqual(check.status, doctor.WARN)
        self.assertIn("--adapter", check.detail)
        self.assertEqual(self.check(report, "mapping X-AoE-Tags").status, doctor.SKIP)

    def test_never_verified_is_a_warning_not_a_failure(self) -> None:
        check = self.check(self.report(), "last verification")
        self.assertEqual(check.status, doctor.WARN)
        self.assertIn("never verified", check.detail)
        self.assertTrue(self.report().ok)


class JsonOutput(DoctorCase):
    def test_json_shape(self) -> None:
        code, out, _ = self.run_cli("doctor", "--json")
        self.assertEqual(code, 0)
        data = json.loads(out)
        self.assertTrue(data["ok"])
        self.assertEqual(data["adapter"], "pi")
        self.assertTrue(Path(data["paths"]["config"]).is_absolute())
        self.assertTrue(all({"check", "status", "detail"} <= set(row) for row in data["checks"]))

    def test_json_reports_failures(self) -> None:
        self.wire(before_session=False)
        code, out, _ = self.run_cli("doctor", "--json")
        self.assertEqual(code, 1)
        data = json.loads(out)
        self.assertFalse(data["ok"])
        failed = [row for row in data["checks"] if row["status"] == "fail"]
        self.assertTrue(failed)


if __name__ == "__main__":  # pragma: no cover
    unittest.main()
