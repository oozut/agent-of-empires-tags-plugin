"""Settings merge and parsing: global + profile, arrays replace, breakage is not fatal."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from llm_tags import settings

from . import helpers


class AppDir(unittest.TestCase):
    def test_xdg_config_home_wins_when_absolute(self) -> None:
        env = {"HOME": "/home/x", "XDG_CONFIG_HOME": "/tmp/cfg"}
        self.assertEqual(settings.app_dir(env), Path("/tmp/cfg/agent-of-empires"))

    def test_relative_xdg_is_ignored(self) -> None:
        env = {"HOME": "/home/x", "XDG_CONFIG_HOME": "relative/cfg"}
        self.assertEqual(settings.app_dir(env), Path("/home/x/.config/agent-of-empires"))

    def test_missing_home_does_not_raise(self) -> None:
        self.assertTrue(str(settings.app_dir({})))


class Load(unittest.TestCase):
    def test_missing_files_are_not_warnings(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = helpers.app_env(Path(tmp))
            raw = settings.load(env=env)
            self.assertEqual(raw.table, {})
            self.assertEqual(raw.warnings, ())
            self.assertEqual(settings.load_config(env=env).present, ())

    def test_profile_overrides_scalars_and_arrays_replace(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = helpers.app_env(Path(tmp))
            global_path = helpers.write_config(
                env,
                helpers.settings_config(
                    'feature_tag = "global"\n'
                    'headers = [{ header = "X-G", template = "g", env_var = "G" }]\n'
                ),
            )
            profile_path = helpers.write_config(
                env,
                helpers.settings_config(
                    'feature_tag = "profile"\n'
                    'headers = [{ header = "X-P", template = "p", env_var = "P" }]\n'
                ),
                profile="main",
            )
            raw = settings.load("main", env=env)
            self.assertEqual(raw.table["feature_tag"], "profile")
            self.assertEqual([row["header"] for row in raw.table["headers"]], ["X-P"])
            self.assertEqual(raw.sources, (str(global_path), str(profile_path)))

    def test_objects_merge_recursively(self) -> None:
        self.assertEqual(
            settings.deep_merge({"a": {"b": 1, "c": 2}}, {"a": {"c": 3}}),
            {"a": {"b": 1, "c": 3}},
        )

    def test_malformed_global_keeps_the_profile(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = helpers.app_env(Path(tmp))
            helpers.write_config(env, "not = = toml\n")
            helpers.write_config(
                env, helpers.settings_config('feature_tag = "profile"'), profile="main"
            )
            raw = settings.load("main", env=env)
            self.assertEqual(raw.table["feature_tag"], "profile")
            self.assertTrue(any("malformed TOML" in warning for warning in raw.warnings))

    def test_malformed_profile_keeps_the_global(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = helpers.app_env(Path(tmp))
            helpers.write_config(env, helpers.settings_config('feature_tag = "global"'))
            helpers.write_config(env, "nope nope\n", profile="main")
            raw = settings.load("main", env=env)
            self.assertEqual(raw.table["feature_tag"], "global")
            self.assertTrue(raw.warnings)

    def test_unknown_plugin_is_ignored(self) -> None:
        self.assertEqual(settings.plugin_table({}), {})
        self.assertEqual(
            settings.plugin_table({"plugins": {"other.plugin": {"settings": {"a": 1}}}}), {}
        )

    def test_host_hook_entries(self) -> None:
        table = {"host_hooks": {"before_session": ["a"], "before_start": "b"}}
        self.assertEqual(settings.host_hook_entries(table, "before_session"), ("a",))
        self.assertEqual(settings.host_hook_entries(table, "before_start"), ("b",))
        self.assertEqual(settings.host_hook_entries(table, "other"), ())
        self.assertEqual(settings.host_hook_entries({}, "before_session"), ())


if __name__ == "__main__":  # pragma: no cover
    unittest.main()
