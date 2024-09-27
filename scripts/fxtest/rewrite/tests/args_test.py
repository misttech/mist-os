# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os
import tempfile
import unittest
import unittest.mock as mock

from parameterized import parameterized

import args
import config

# We need a place to create a temporary file that is used to ensure
# output directory checking works correctly. Place it here and then
# cleanup after the tests are done executing.
GLOBAL_TEMP_DIRECTORY = tempfile.TemporaryDirectory()
GLOBAL_FILE_NAME = os.path.join(GLOBAL_TEMP_DIRECTORY.name, "tempfile")
open(GLOBAL_FILE_NAME, "w").close()


class TestArgs(unittest.TestCase):
    @classmethod
    def tearDownClass(cls) -> None:
        GLOBAL_TEMP_DIRECTORY.cleanup()
        return super().tearDownClass()

    def test_empty(self) -> None:
        flags = args.parse_args([])
        flags.validate()

    @parameterized.expand(
        [
            (
                "cannot show status with --simple",
                ["--simple", "--status"],
            ),
            (
                "cannot show style with --simple",
                ["--simple", "--style"],
            ),
            (
                "cannot show status when terminal is not a TTY",
                ["--status"],
            ),
            (
                "cannot run only host and only device tests",
                ["--device", "--host"],
            ),
            (
                "cannot exceed maximum status frequency",
                ["--status-delay", ".00001"],
            ),
            (
                "cannot have a negative suggestion count",
                ["--suggestion-count", "-1"],
            ),
            (
                "cannot run tests 0 times",
                ["--count", "0"],
            ),
            (
                "cannot have a negative timeout",
                ["--timeout", "-3"],
            ),
            (
                "cannot output to a file",
                ["--ffx-output-directory", GLOBAL_FILE_NAME],
            ),
            (
                "cannot output to a file (artifact flag)",
                ["--artifact-output-directory", GLOBAL_FILE_NAME],
            ),
            (
                "cannot output to a file (outdir flag)",
                ["--outdir", GLOBAL_FILE_NAME],
            ),
            (
                "cannot set a negative --parallel",
                ["--parallel", "-1"],
            ),
            (
                "cannot set a negative --parallel-cases",
                ["--parallel-cases", "-1"],
            ),
            (
                "invalid environment variable formatting is checked",
                ["-e", "abcd"],
            ),
            (
                "--break-on-failure and --breakpoint flags are not supported with host tests.",
                ["--break-on-failure", "--host"],
            ),
            (
                "--break-on-failure and --breakpoint flags are not supported with host tests.",
                ["--breakpoint", "test.cc:123", "--host"],
            ),
        ]
    )
    @mock.patch("args.termout.is_valid", return_value=False)
    def test_validation_errors(
        self, _unused_name: str, arg_list: list[str], _mock: mock.Mock
    ) -> None:
        flags = args.parse_args(arg_list)
        try:
            with self.assertRaises(args.FlagError):
                flags.validate()
        except AssertionError:
            raise AssertionError("Expected FlagError from " + str(arg_list))

    def test_simple(self) -> None:
        flags = args.parse_args(["--simple"])
        flags.validate()
        self.assertEqual(flags.style, False)
        self.assertEqual(flags.status, False)

    def test_e2e(self) -> None:
        flags = args.parse_args(["--only-e2e"])
        flags.validate()
        self.assertEqual(flags.e2e, True)
        self.assertEqual(flags.only_e2e, True)

    def test_use_test_interface(self) -> None:
        flags = args.parse_args(["--use-test-interface"])
        flags.validate()
        self.assertEqual(flags.use_test_interface, True)

        flags = args.parse_args([])
        flags.validate()
        self.assertEqual(flags.use_test_interface, False)

    def test_expand_output_variable(self) -> None:
        flags = args.parse_args(["--outdir", "$FUCHSIA_OUT/test_out"])
        flags.validate()
        flags.update_artifacts_directory_with_out_path("out/default")
        self.assertEqual(
            flags.artifact_output_directory, "out/default/test_out"
        )

        flags = args.parse_args(["--outdir", "${FUCHSIA_OUT}/test_out2"])
        flags.validate()
        flags.update_artifacts_directory_with_out_path("out/default2")
        self.assertEqual(
            flags.artifact_output_directory, "out/default2/test_out2"
        )

    def test_default_merging(self) -> None:
        config_file = config.ConfigFile(
            "path", args.parse_args(["--parallel=10"])
        )
        flags = args.parse_args([], config_file.default_flags)
        self.assertEqual(flags.parallel, 10)
        flags = args.parse_args(["--parallel=1"], config_file.default_flags)
        self.assertEqual(flags.parallel, 1)

    def test_selections_after_test_filter(self) -> None:
        """Passing more selections after a --test-filter works"""
        flags = args.parse_args(["foo", "--test-filter", "some*", "bar"])
        flags.validate()
        self.assertListEqual(flags.test_filter, ["some*"])
        self.assertListEqual(flags.selection, ["foo", "bar"])

        flags = args.parse_args(
            [
                "foo",
                "--test-filter",
                "some*",
                "bar",
                "-a",
                "baz",
                "alpha",
                "--test-filter",
                "something*",
                "beta",
                "-c",
                "gamma.cm",
            ]
        )
        flags.validate()
        self.assertListEqual(flags.test_filter, ["some*", "something*"])
        self.assertListEqual(
            flags.selection,
            [
                "foo",
                "bar",
                "--and",
                "baz",
                "alpha",
                "beta",
                "--component",
                "gamma.cm",
            ],
        )

    def test_exact_after_selections(self) -> None:
        """Passing --exact after a selection works"""
        flags = args.parse_args(["-p", "foo", "-a", "-c", "bar", "--exact"])
        flags.validate()
        self.assertEqual(flags.exact, True)
        self.assertListEqual(
            flags.selection,
            ["--package", "foo", "--and", "--component", "bar"],
        )

    @parameterized.expand(
        [
            ("default is None", [], [], None),
            ("config file overrides output", [], ["--output"], True),
            ("-o shows output", ["-o"], [], True),
            ("--output shows output", ["--output"], [], True),
            ("--no-output hides output", ["--no-output"], [], False),
            (
                "--no-output overrides config",
                ["--no-output"],
                ["--output"],
                False,
            ),
        ]
    )
    def test_output_toggle(
        self,
        _unused_name: str,
        arguments: list[str],
        config_arguments: list[str],
        expected_value: bool,
    ) -> None:
        config_file = config.ConfigFile(
            "path", args.parse_args(config_arguments)
        )
        flags = args.parse_args(arguments, config_file.default_flags)
        self.assertEqual(flags.output, expected_value)
