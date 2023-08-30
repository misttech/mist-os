# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import json
import os
import subprocess
import tempfile
import typing
import unittest

from parameterized import parameterized

import args
import environment
import event
import execution
import test_list_file
import tests_json_file


class TestExecution(unittest.IsolatedAsyncioTestCase):
    def assertContainsSublist(
        self, target: typing.List[typing.Any], data: typing.List[typing.Any]
    ):
        """Helper method to assert that one list is contained in the other, in order.

        Args:
            target (typing.List[typing.Any]): The sublist to search for.
            data (typing.List[typing.Any]): The list to search in.
        """
        self.assertNotEqual(len(target), 0, "Target list cannot be empty")
        starts = [i for i, v in enumerate(data) if v == target[0]]
        for start_index in starts:
            if data[start_index : start_index + len(target)] == target:
                return
        self.assertTrue(False, f"List {target} is not a sublist of {data}")

    async def test_run_command(self):
        """Test that run_command works with and without events"""
        with tempfile.TemporaryDirectory() as tmp:
            open(os.path.join(tmp, "temp-file.txt"), "w").close()

            output = await execution.run_command(
                "ls", "temp-file.txt", env={"CWD": tmp}
            )
            assert output is not None
            self.assertEqual(output.return_code, 0)

            recorder = event.EventRecorder()
            recorder.emit_init()

            output = await execution.run_command(
                "ls", "temp-file.txt", env={"CWD": tmp}, recorder=recorder
            )
            assert output is not None
            self.assertEqual(output.return_code, 0)

            recorder.emit_end()

            events = [e async for e in recorder.iter()]
            # Ensure we got init, end, and at least one sub-event start/stop
            self.assertGreater(len(events), 4)

    async def test_run_command_failure(self):
        """Test that running an invalid command emits an error event"""
        recorder = event.EventRecorder()
        recorder.emit_init()
        output = await execution.run_command(
            "___invalid_command_name___", recorder=recorder
        )
        recorder.emit_end()
        events = [e async for e in recorder.iter()]

        # Ensure we get no output and that at least one event is an error.
        self.assertIsNone(output)
        self.assertTrue(any([e.error is not None for e in events]))

    @parameterized.expand(
        [
            (
                "with default log severity",
                [],
                [["--max-severity-logs", "INFO"]],
                [],
            ),
            (
                "without log restriction",
                ["--no-restrict-logs"],
                [],
                ["--max-severity-logs"],
            ),
            (
                "with default to not run disabled tests",
                [],
                [],
                ["--run-disabled"],
            ),
            (
                "with disabled tests running",
                ["--also-run-disabled-tests"],
                [["--run-disabled"]],
                [],
            ),
        ]
    )
    async def test_test_execution_component(
        self,
        _unused_name,
        flag_list: typing.List[str],
        expected_present_flag_lists: typing.List[typing.List[str]],
        expected_not_present_flags: typing.List[str],
    ):
        """Test the usage of the TestExecution wrapper on a component test"""

        exec_env = environment.ExecutionEnvironment(
            "/fuchsia", "/out/fuchsia", None, "", ""
        )

        test = execution.TestExecution(
            test_list_file.Test(
                tests_json_file.TestEntry(tests_json_file.TestSection("foo", "//foo")),
                test_list_file.TestListEntry(
                    "foo",
                    [],
                    test_list_file.TestListExecutionEntry(
                        "fuchsia-pkg://fuchsia.com/foo#meta/foo_test.cm",
                        realm="foo_tests",
                        max_severity_logs="INFO",
                        min_severity_logs="TRACE",
                    ),
                ),
            ),
            exec_env,
            args.parse_args(flag_list + ["--no-use-package-hash"]),
        )

        command_line = test.command_line()
        self.assertContainsSublist(
            ["fx", "ffx", "test", "run", "--realm", "foo_tests"], command_line
        )
        self.assertContainsSublist(["--min-severity-logs", "TRACE"], command_line)
        self.assertContainsSublist(
            ["fuchsia-pkg://fuchsia.com/foo#meta/foo_test.cm"], command_line
        )

        for expected_flag_list in expected_present_flag_lists:
            self.assertContainsSublist(expected_flag_list, command_line)
        for not_present_flag in expected_not_present_flags:
            self.assertFalse(
                not_present_flag in command_line,
                f"Expected {not_present_flag} to not be in {command_line}",
            )

        self.assertFalse(test.is_hermetic())
        self.assertIsNone(test.environment())
        self.assertTrue(test.should_symbolize())

    async def test_test_execution_component_parallel(self):
        """Test the usage of the TestExecution wrapper on a component test with a parallel override"""

        exec_env = environment.ExecutionEnvironment(
            "/fuchsia", "/out/fuchsia", None, "", ""
        )

        test = execution.TestExecution(
            test_list_file.Test(
                tests_json_file.TestEntry(
                    tests_json_file.TestSection("foo", "//foo", parallel=1)
                ),
                test_list_file.TestListEntry(
                    "foo",
                    [],
                    test_list_file.TestListExecutionEntry(
                        "fuchsia-pkg://fuchsia.com/foo#meta/foo_test.cm",
                        realm="foo_tests",
                        max_severity_logs="INFO",
                        min_severity_logs="TRACE",
                    ),
                ),
            ),
            exec_env,
            args.parse_args(["--no-use-package-hash"]),
        )

        self.assertListEqual(
            test.command_line(),
            [
                "fx",
                "ffx",
                "test",
                "run",
                "--realm",
                "foo_tests",
                "--max-severity-logs",
                "INFO",
                "--min-severity-logs",
                "TRACE",
                "--parallel",
                "1",
                "fuchsia-pkg://fuchsia.com/foo#meta/foo_test.cm",
            ],
        )

        self.assertFalse(test.is_hermetic())
        self.assertIsNone(test.environment())
        self.assertTrue(test.should_symbolize())

    async def test_test_execution_host(self):
        """Test the usage of the TestExecution wrapper on a host test, and actually run it"""

        with tempfile.TemporaryDirectory() as tmp:
            # We will run ls, but it needs to be relative to the output directory.
            # Find the actual path to the ls binary and symlink it into the
            # output directory.
            ls_path = subprocess.check_output(["which", "ls"]).decode().strip()
            self.assertTrue(os.path.isfile, f"{ls_path} is not a file")
            os.symlink(ls_path, os.path.join(tmp, "ls"))

            exec_env = environment.ExecutionEnvironment("/fuchsia", tmp, None, "", "")

            flags = args.parse_args([])

            test = execution.TestExecution(
                test_list_file.Test(
                    tests_json_file.TestEntry(
                        tests_json_file.TestSection("foo", "//foo", path="ls")
                    ),
                    test_list_file.TestListEntry("foo", [], execution=None),
                ),
                exec_env,
                flags,
            )

            self.assertFalse(test.is_hermetic())
            env = test.environment()
            assert env is not None
            self.assertDictEqual(env, {"CWD": tmp})
            self.assertFalse(test.should_symbolize())

            recorder = event.EventRecorder()
            recorder.emit_init()

            output = await test.run(recorder, flags, event.GLOBAL_RUN_ID)
            recorder.emit_end()

            assert output is not None

            self.assertFalse(any([e.error is not None async for e in recorder.iter()]))

    async def test_test_execution_with_package_hash(self):
        """Ensure that test execution respects --use-package-hash"""
        with tempfile.TemporaryDirectory() as tmp:
            with open(os.path.join(tmp, "package-repositories.json"), "w") as f:
                json.dump(
                    [
                        {
                            "targets": "targets.json",
                        }
                    ],
                    f,
                )
            with open(os.path.join(tmp, "targets.json"), "w") as f:
                json.dump(
                    {
                        "signed": {
                            "targets": {
                                "foo_test/0": {
                                    "custom": {
                                        "merkle": "f00",
                                    }
                                },
                                "bar_test/0": {},
                            }
                        }
                    },
                    f,
                )

            def make_test(name: str) -> test_list_file.Test:
                name = f"fuchsia-pkg://fuchsia.com/{name}#meta/test.cm"
                return test_list_file.Test(
                    tests_json_file.TestEntry(
                        tests_json_file.TestSection(name, "//foo")
                    ),
                    test_list_file.TestListEntry(
                        name, [], execution=test_list_file.TestListExecutionEntry(name)
                    ),
                )

            def assertTestExecutionFailsUsingMerkleHash(
                error_regex: str,
                test: test_list_file.Test,
                exec_env: environment.ExecutionEnvironment,
            ):
                self.assertRaisesRegex(
                    execution.TestCouldNotRun,
                    error_regex,
                    lambda: execution.TestExecution(
                        test, exec_env, args.parse_args([])
                    ).command_line(),
                )
                self.assertIsNotNone(
                    execution.TestExecution(
                        test, exec_env, args.parse_args(["--no-use-package-hash"])
                    ).command_line()
                )

            # This environment is missing a package-repository.json file, so attempts
            # to match a merkle root will fail.
            missing_exec_env = environment.ExecutionEnvironment(
                "/fuchsia", "", "", "", ""
            )

            assertTestExecutionFailsUsingMerkleHash(
                "Could not load a Merkle hash",
                make_test("foo_test"),
                missing_exec_env,
            )

            # This environment contains a package repository, so we need to
            # ensure the merkle hash argument is respected.
            exec_env = environment.ExecutionEnvironment(
                "/fuchsia",
                "",
                "",
                "",
                "",
                package_repositories_file=os.path.join(
                    tmp, "package-repositories.json"
                ),
            )

            # Hash is present only when use_merkle_hash is True, absent otherwise.
            self.assertIn(
                "fuchsia-pkg://fuchsia.com/foo_test?hash=f00#meta/test.cm",
                execution.TestExecution(
                    make_test("foo_test"), exec_env, args.parse_args([])
                ).command_line(),
            )
            self.assertIn(
                "fuchsia-pkg://fuchsia.com/foo_test#meta/test.cm",
                execution.TestExecution(
                    make_test("foo_test"),
                    exec_env,
                    args.parse_args(["--no-use-package-hash"]),
                ).command_line(),
            )

            # Mangle the component URL such that a name cannot be extracted,
            # and expect an error.
            broken_test = make_test("foo_test")
            assert broken_test.info is not None
            assert broken_test.info.execution is not None
            broken_test.info.execution.component_url = "foo_test"

            assertTestExecutionFailsUsingMerkleHash(
                "Failed to parse package name", broken_test, exec_env
            )

            # This test has an entry, but no merkle.
            assertTestExecutionFailsUsingMerkleHash(
                "Could not find a Merkle hash for this test",
                make_test("bar_test"),
                exec_env,
            )

            # This test has no entry.
            assertTestExecutionFailsUsingMerkleHash(
                "Could not find a Merkle hash for this test",
                make_test("baz_test"),
                exec_env,
            )
