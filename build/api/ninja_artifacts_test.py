# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os
import sys
import tempfile
import typing as T
import unittest
from pathlib import Path

sys.path.insert(0, Path(__file__).parent)
import ninja_artifacts


class MockNinjaRunner(object):
    def __init__(self, mock_output: str) -> None:
        self.command_args: T.Sequence[str] = []
        self.command_build_dir = ""
        self._mock_output = mock_output

    def run_and_extract_output(
        self, build_dir: str, cmd_args: T.Sequence[str]
    ) -> str:
        self.command_build_dir = build_dir
        self.command_args = cmd_args
        return self._mock_output


class NinjaArtifactsTest(unittest.TestCase):
    def test_get_last_build_targets(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            build_dir = Path(temp_dir)

            # If the file doesn't exist, default to [":default"].
            self.assertListEqual(
                ninja_artifacts.get_last_build_targets(build_dir), [":default"]
            )

            # If the file is empty, default to [":default"] too.
            (build_dir / ninja_artifacts.LAST_NINJA_TARGETS_FILE).write_text("")
            self.assertListEqual(
                ninja_artifacts.get_last_build_targets(build_dir), [":default"]
            )

            (build_dir / ninja_artifacts.LAST_NINJA_TARGETS_FILE).write_text(
                " foo"
            )
            self.assertListEqual(
                ninja_artifacts.get_last_build_targets(build_dir), ["foo"]
            )

            (build_dir / ninja_artifacts.LAST_NINJA_TARGETS_FILE).write_text(
                "foo bar"
            )
            self.assertListEqual(
                ninja_artifacts.get_last_build_targets(build_dir),
                ["foo", "bar"],
            )

    def test_get_build_plan_deps(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            build_dir = Path(temp_dir)

            (build_dir / ninja_artifacts.NINJA_BUILD_PLAN_DEPS_FILE).write_text(
                "build.ninja.stamp: dep1 dep2 dep3 dep4\n"
            )

            self.assertListEqual(
                ninja_artifacts.get_build_plan_deps(build_dir),
                ["dep1", "dep2", "dep3", "dep4"],
            )

    def test_check_output_needs_update(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            build_dir = Path(temp_dir)

            input_files = [build_dir / "input1", build_dir / "input2"]
            output_file = build_dir / "output"

            # output file does not exist, nor any input file.
            self.assertTrue(
                ninja_artifacts.check_output_needs_update(
                    output_file, input_files
                )
            )

            # output file does not exist, but input files do.
            input_files[0].write_text("one")
            input_files[1].write_text("two")
            self.assertTrue(
                ninja_artifacts.check_output_needs_update(
                    output_file, input_files
                )
            )

            # output file does exist, and is newer than inputs.
            output_file.write_text("out")
            self.assertFalse(
                ninja_artifacts.check_output_needs_update(
                    output_file, input_files
                )
            )

            # output file does exist, but is older than one input.
            output_stat = output_file.stat()
            os.utime(
                input_files[1],
                times=(output_stat.st_atime, output_stat.st_mtime + 1),
            )
            self.assertTrue(
                ninja_artifacts.check_output_needs_update(
                    output_file, input_files
                )
            )

    def test_get_last_build_artifacts(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            # Setup fake source and build directory.
            build_gn_path = Path(temp_dir) / "BUILD.gn"
            build_gn_path.write_text("# Fake BUILD.gn\n")

            build_dir = Path(temp_dir) / "out"
            build_dir.mkdir(parents=True)

            build_ninja_d_path = (
                build_dir / ninja_artifacts.NINJA_BUILD_PLAN_DEPS_FILE
            )
            build_ninja_d_path.write_text("build.ninja.stamp: ../BUILD.gn")

            last_targets_path = (
                build_dir / ninja_artifacts.LAST_NINJA_TARGETS_FILE
            )
            last_targets_path.write_text("foo")

            # Create mock NinjaRunner instance to avoid calling Ninja binary.
            ninja_runner = MockNinjaRunner("bar\nfoo\nzoo\n'quoted'\n")
            self.assertListEqual(
                ninja_artifacts.get_last_build_artifacts(
                    build_dir, ninja_runner
                ),
                ["bar", "foo", "zoo", "'quoted'"],
            )
            self.assertEqual(ninja_runner.command_build_dir, str(build_dir))
            self.assertListEqual(
                ninja_runner.command_args, ["-t", "outputs", "foo"]
            )

            last_ninja_artifacts_path = (
                build_dir / ninja_artifacts.LAST_NINJA_ARTIFACTS_FILE
            )
            self.assertTrue(last_ninja_artifacts_path.exists())
            self.assertEqual(
                last_ninja_artifacts_path.read_text(), "bar\nfoo\nzoo\n'quoted'"
            )

            # Modify last_ninja_build_targets.txt and verify the cache was regenerated.

            last_targets_path.write_text("bar zoo")
            last_targets_stat = last_targets_path.stat()
            os.utime(
                last_targets_path,
                times=(
                    last_targets_stat.st_atime,
                    last_targets_stat.st_mtime + 1,
                ),
            )

            ninja_runner = MockNinjaRunner("second\ncall\n")
            self.assertListEqual(
                ninja_artifacts.get_last_build_artifacts(
                    build_dir, ninja_runner
                ),
                ["second", "call"],
            )
            self.assertEqual(ninja_runner.command_build_dir, str(build_dir))
            self.assertListEqual(
                ninja_runner.command_args, ["-t", "outputs", "bar", "zoo"]
            )
            self.assertEqual(
                last_ninja_artifacts_path.read_text(), "second\ncall"
            )

    def test_get_last_build_sources(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            # Setup fake source and build directory.
            build_gn_path = Path(temp_dir) / "BUILD.gn"
            build_gn_path.write_text("# Fake BUILD.gn\n")

            build_dir = Path(temp_dir) / "out"
            build_dir.mkdir(parents=True)

            build_ninja_d_path = (
                build_dir / ninja_artifacts.NINJA_BUILD_PLAN_DEPS_FILE
            )
            build_ninja_d_path.write_text("build.ninja.stamp: ../BUILD.gn")

            last_targets_path = (
                build_dir / ninja_artifacts.LAST_NINJA_TARGETS_FILE
            )
            last_targets_path.write_text("foo")

            # Create mock NinjaRunner instance to avoid calling Ninja binary.
            ninja_runner = MockNinjaRunner(
                "../src/foo\n../src/bar\noutput_file\nout_dir/out_file\n../src/zoo\n"
            )
            self.assertListEqual(
                ninja_artifacts.get_last_build_sources(build_dir, ninja_runner),
                ["../src/foo", "../src/bar", "../src/zoo"],
            )
            self.assertEqual(ninja_runner.command_build_dir, str(build_dir))
            self.assertListEqual(
                ninja_runner.command_args,
                [
                    "-t",
                    "inputs",
                    "--no-shell-escape",
                    "--dependency-order",
                    "foo",
                ],
            )

            last_ninja_sources_path = (
                build_dir / ninja_artifacts.LAST_NINJA_SOURCES_FILE
            )
            self.assertTrue(last_ninja_sources_path.exists())
            self.assertEqual(
                last_ninja_sources_path.read_text(),
                "../src/foo\n../src/bar\n../src/zoo",
            )

            # Modify last_ninja_build_targets.txt and verify the cache was regenerated.

            last_targets_path.write_text("bar zoo")
            last_targets_stat = last_targets_path.stat()
            os.utime(
                last_targets_path,
                times=(
                    last_targets_stat.st_atime,
                    last_targets_stat.st_mtime + 1,
                ),
            )

            ninja_runner = MockNinjaRunner("../second\n../call\n")
            self.assertListEqual(
                ninja_artifacts.get_last_build_sources(build_dir, ninja_runner),
                ["../second", "../call"],
            )
            self.assertEqual(ninja_runner.command_build_dir, str(build_dir))
            self.assertListEqual(
                ninja_runner.command_args,
                [
                    "-t",
                    "inputs",
                    "--no-shell-escape",
                    "--dependency-order",
                    "bar",
                    "zoo",
                ],
            )
            self.assertEqual(
                last_ninja_sources_path.read_text(), "../second\n../call"
            )


if __name__ == "__main__":
    unittest.main()
