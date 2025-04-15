#!/usr/bin/env fuchsia-vendored-python
# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os
import sys
import typing as T
import unittest

sys.path.insert(0, os.path.dirname(__file__))

from gn_targets_utils import (
    BazelBuildActionQuery,
    BazelBuildActionsMap,
    find_gn_bazel_action_infos_for,
)


class BazelBuildActionsMapTest(unittest.TestCase):
    _JSON_INPUT = [
        {
            "bazel_targets": [
                "//bazel/target:1",
                "//bazel/target:2",
            ],
            "gn_target": "//some/gn:target_1",
            "gn_targets_dir": "obj/some/gn/target_1.gn_targets",
            "gn_targets_manifest": "gen/some/gn/target_1.gn_targets.json",
            "no_sdk": False,
            "bazel_command_file": "obj/some/gn/target_1.bazel_command.sh",
            "build_events_log_json": "obj/some/gn/target_1.events_log.json",
            "path_mapping": "obj/some/gn/target_1.path_mapping",
        },
        {
            "bazel_targets": [
                "//another/target:3",
                "//another/target:4",
            ],
            "gn_target": "//some/gn:target_2",
            "gn_targets_dir": "obj/some/gn/target_2.gn_targets",
            "gn_targets_manifest": "gen/some/gn/target_2.gn_targets.json",
            "no_sdk": False,
            "bazel_command_file": "obj/some/gn/target_2.bazel_command.sh",
            "build_events_log_json": "obj/some/gn/target_2.events_log.json",
            "path_mapping": "obj/some/gn/target_2.path_mapping",
        },
    ]

    def test_actions_map(self) -> None:
        actions_map = BazelBuildActionsMap(self._JSON_INPUT)

        self.assertListEqual(
            actions_map.bazel_targets,
            [
                "//another/target:3",
                "//another/target:4",
                "//bazel/target:1",
                "//bazel/target:2",
            ],
        )

        self.assertEqual(
            actions_map.find_gn_target_for("//bazel/target:1"),
            "//some/gn:target_1",
        )

        self.assertEqual(
            actions_map.find_gn_target_for("//bazel/target:2"),
            "//some/gn:target_1",
        )

        self.assertEqual(
            actions_map.find_gn_target_for("//another/target:3"),
            "//some/gn:target_2",
        )

        self.assertEqual(
            actions_map.find_gn_target_for("//another/target:4"),
            "//some/gn:target_2",
        )

        self.assertEqual(actions_map.find_gn_target_for("//does/not:exist"), "")

        info = actions_map.get_info("//some/gn:target_1")
        self.assertEqual(info.gn_target, "//some/gn:target_1")
        self.assertListEqual(
            info.bazel_targets,
            [
                "//bazel/target:1",
                "//bazel/target:2",
            ],
        )
        self.assertFalse(info.no_sdk)
        self.assertEqual(
            info.gn_targets_dir,
            "obj/some/gn/target_1.gn_targets",
        )
        self.assertEqual(
            info.gn_targets_manifest, "gen/some/gn/target_1.gn_targets.json"
        )

        self.assertEqual(actions_map.get_info("//does/not:exist"), None)

    def test_query(self) -> None:
        actions_map = BazelBuildActionsMap(self._JSON_INPUT)

        self.maxDiff = None

        action_query = BazelBuildActionQuery("//some:target", actions_map)

        self.assertListEqual(
            action_query.make_query_command(["bazel"]),
            [
                "bazel",
                "query",
                "--config=no_gn_targets",
                "--config=quiet",
                "--keep_going",
                r"""allpaths(set(//another/target:3 //another/target:4 //bazel/target:1 //bazel/target:2), //some:target)""",
            ],
        )

        self.assertListEqual(action_query.process_query_output(""), [])

        query_output = r"""//another/target:3
//intermediate/target:path
//other:thingy
//some:target
"""
        self.assertListEqual(
            action_query.process_query_output(query_output),
            ["//some/gn:target_2"],
        )

        self.assertEqual(action_query.filter_query_errors(""), "")

        self.assertEqual(
            action_query.filter_query_errors(
                r"""ERROR: /work/space/foo/BUILD.bazel: no such package '@@gn_targets//src/foo:gn_artifact
ERROR: Evaluation of query "allpaths(set(//src/foo:bazel_target_1 //src/foo:bazel_target_2), //src/foo/lib:target)" failed.
"""
            ),
            "",
        )

        self.assertEqual(
            action_query.filter_query_errors(
                r"""ERROR: /work/space/foo/BUILD.bazel: no such package '@@gn_targets//src/foo:gn_artifact
ERROR: This is an unrelated error message
ERROR: And this is another one
ERROR: Evaluation of query "allpaths(set(//src/foo:bazel_target_1 //src/foo:bazel_target_2), //src/foo/lib:target)" failed.
"""
            ),
            "ERROR: This is an unrelated error message\nERROR: And this is another one",
        )


class FindGnBazelActioInfosForTest(unittest.TestCase):
    _JSON_INPUT = [
        {
            "bazel_targets": [
                "//bazel/target:1",
                "//bazel/target:2",
            ],
            "gn_target": "//some/gn:target_1",
            "gn_targets_dir": "obj/some/gn/target_1.gn_targets",
            "gn_targets_manifest": "gen/some/gn/target_1.gn_targets.json",
            "no_sdk": False,
            "bazel_command_file": "obj/some/gn/target_1.bazel_command.sh",
            "build_events_log_json": "obj/some/gn/target_1.events_log.json",
            "path_mapping": "obj/some/gn/target_1.path_mapping",
        },
        {
            "bazel_targets": [
                "//another/target:3",
                "//another/target:4",
            ],
            "gn_target": "//some/gn:target_2",
            "gn_targets_dir": "obj/some/gn/target_2.gn_targets",
            "gn_targets_manifest": "gen/some/gn/target_2.gn_targets.json",
            "no_sdk": False,
            "bazel_command_file": "obj/some/gn/target_2.bazel_command.sh",
            "build_events_log_json": "obj/some/gn/target_2.events_log.json",
            "path_mapping": "obj/some/gn/target_2.path_mapping",
        },
    ]

    def setUp(self) -> None:
        self.actions_map = BazelBuildActionsMap(self._JSON_INPUT)
        self.errors: list[str] = []

    def tearDown(self) -> None:
        pass

    @property
    def log_err(self) -> T.Callable[[str], None]:
        def _log_err(msg: str) -> None:
            self.errors.append(msg)

        return _log_err

    def test_malformed_bazel_targets(self) -> None:
        self.errors = []
        result = find_gn_bazel_action_infos_for(
            "no_leading_slashes:target",
            self.actions_map,
            ["bazel"],
            log_err=self.log_err,
        )

        self.assertListEqual(result, [])
        self.assertListEqual(
            self.errors,
            ["Target label must start with // or @: no_leading_slashes:target"],
        )

        self.errors = []
        result = find_gn_bazel_action_infos_for(
            "//gn/label(//with:toolchain)",
            self.actions_map,
            ["bazel"],
            log_err=self.log_err,
        )
        self.assertListEqual(result, [])
        self.assertListEqual(
            self.errors,
            [
                "Target label cannot include GN toolchain suffix: //gn/label(//with:toolchain)"
            ],
        )

    def test_direct_mapping(self) -> None:
        def command_runner(args: T.List[str]) -> T.Tuple[int, str, str]:
            return 1, "", "An unexpected error\nAnd another one\n"

        result = find_gn_bazel_action_infos_for(
            "//bazel/target:1",
            self.actions_map,
            ["bazel"],
            log_err=self.log_err,
            command_runner=command_runner,
        )

        self.assertListEqual(
            result, [self.actions_map.get_info("//some/gn:target_1")]
        )
        self.assertListEqual(self.errors, [])

    def test_single_dependency(self) -> None:
        def command_runner(args: T.List[str]) -> T.Tuple[int, str, str]:
            return (
                1,
                r"""//bazel/target/dependency:5
//bazel/some/other:dependency
//bazel/target:1
""",
                r"""Starting local Bazel server and connecting to it...
WARNING: --keep_going specified, ignoring errors.
ERROR: /tmp/work: no such package '@@gn_targets//src/foo:bar' ...
ERROR: Evaluation of query "allpaths(set(...), //bazel/target/dependency:5) failed.
""",
            )

        result = find_gn_bazel_action_infos_for(
            "//bazel/target/dependency:5",
            self.actions_map,
            ["bazel"],
            log_err=self.log_err,
            command_runner=command_runner,
        )

        self.assertListEqual(self.errors, [])

        self.assertListEqual(
            result, [self.actions_map.get_info("//some/gn:target_1")]
        )

    def test_multiple_dependencies_same_gn_action(self) -> None:
        def command_runner(args: T.List[str]) -> T.Tuple[int, str, str]:
            return (
                1,
                r"""//bazel/target/dependency:5
//bazel/some/other:dependency
//bazel/target:1
//bazel/target:2
""",
                r"""Starting local Bazel server and connecting to it...
WARNING: --keep_going specified, ignoring errors.
ERROR: /tmp/work: no such package '@@gn_targets//src/foo:bar' ...
ERROR: Evaluation of query "allpaths(set(...), //bazel/target/dependency:5) failed.
""",
            )

        result = find_gn_bazel_action_infos_for(
            "//bazel/target/dependency:5",
            self.actions_map,
            ["bazel"],
            log_err=self.log_err,
            command_runner=command_runner,
        )

        self.assertListEqual(self.errors, [])

        self.assertListEqual(
            result, [self.actions_map.get_info("//some/gn:target_1")]
        )

    def test_multiple_dependencies(self) -> None:
        def command_runner(args: T.List[str]) -> T.Tuple[int, str, str]:
            return (
                1,
                r"""//bazel/target/dependency:5
//bazel/some/other:dependency
//bazel/target:1
//bazel/extra:dependency
//another/target:4
""",
                r"""Starting local Bazel server and connecting to it...
WARNING: --keep_going specified, ignoring errors.
ERROR: /tmp/work: no such package '@@gn_targets//src/foo:bar' ...
ERROR: Evaluation of query "allpaths(set(...), //bazel/target/dependency:5) failed.
""",
            )

        result = find_gn_bazel_action_infos_for(
            "//bazel/target/dependency:5",
            self.actions_map,
            ["bazel"],
            log_err=self.log_err,
            command_runner=command_runner,
        )

        self.assertListEqual(self.errors, [])

        self.assertListEqual(
            result,
            [
                self.actions_map.get_info("//some/gn:target_1"),
                self.actions_map.get_info("//some/gn:target_2"),
            ],
        )

    def test_unknown_dependency(self) -> None:
        def command_runner(args: T.List[str]) -> T.Tuple[int, str, str]:
            return 0, "", ""

        result = find_gn_bazel_action_infos_for(
            "//bazel/dependency:unknown",
            self.actions_map,
            ["bazel"],
            log_err=self.log_err,
            command_runner=command_runner,
        )

        self.assertListEqual(result, [])
        self.assertListEqual(self.errors, [])

    def test_unexpected_query_errors(self) -> None:
        def command_runner(args: T.List[str]) -> T.Tuple[int, str, str]:
            return 1, "", "An unexpected error\nAnd another one\n"

        result = find_gn_bazel_action_infos_for(
            "//bazel/target/dependency:5",
            self.actions_map,
            ["bazel"],
            log_err=self.log_err,
            command_runner=command_runner,
        )

        self.assertListEqual(result, [])
        self.assertListEqual(
            self.errors,
            [
                r"""Bazel query returned unexpected errors:
An unexpected error
And another one
"""
            ],
        )


if __name__ == "__main__":
    unittest.main()
