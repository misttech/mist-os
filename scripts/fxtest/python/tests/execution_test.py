# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import json
import os
import subprocess
import tempfile
import typing
import unittest
import unittest.mock as mock

import async_utils.command as command
from parameterized import parameterized

import args
import environment
import event
import execution
import test_list_file
import tests_json_file


def _make_exec_env(
    fuchsia_dir: str, out_dir: str
) -> environment.ExecutionEnvironment:
    """Create an execution environment for test."""
    return environment.ExecutionEnvironment(
        fuchsia_dir,
        out_dir,
        test_json_file="",
        log_file=None,
        test_list_file="",
    )


class TestExecution(unittest.IsolatedAsyncioTestCase):
    def assertContainsSublist(
        self, target: list[typing.Any], data: list[typing.Any]
    ) -> None:
        """Helper method to assert that one list is contained in the other, in order.

        Args:
            target (list[typing.Any]): The sublist to search for.
            data (list[typing.Any]): The list to search in.
        """
        self.assertNotEqual(len(target), 0, "Target list cannot be empty")
        starts = [i for i, v in enumerate(data) if v == target[0]]
        for start_index in starts:
            if data[start_index : start_index + len(target)] == target:
                return
        self.assertTrue(False, f"List {target} is not a sublist of {data}")

    async def test_run_command(self) -> None:
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

    async def test_run_command_failure(self) -> None:
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
                "with min log severity override",
                ["--min-severity-logs", "DEBUG"],
                [["--min-severity-logs", "DEBUG"]],
                [],
            ),
            (
                "without log restriction",
                ["--no-restrict-logs"],
                [["--min-severity-logs", "TRACE"]],
                ["--max-severity-logs"],
            ),
            (
                "with default to not run disabled tests",
                [],
                [["--min-severity-logs", "TRACE"]],
                ["--run-disabled"],
            ),
            (
                "with disabled tests running",
                ["--also-run-disabled-tests"],
                [["--run-disabled"], ["--min-severity-logs", "TRACE"]],
                [],
            ),
            (
                "with full moniker in logs",
                ["--show-full-moniker-in-logs"],
                [["--show-full-moniker-in-logs"]],
                [],
            ),
            (
                "without full moniker in logs",
                ["--no-show-full-moniker-in-logs"],
                [],
                ["--show-full-moniker-in-logs"],
            ),
            (
                "without ffx output directory",
                [],
                [],
                ["--output-directory"],
            ),
            (
                "with ffx output directory",
                ["--ffx-output-directory", "foo"],
                [["--output-directory", "foo/0"]],
                [],
            ),
            (
                "with extra args",
                ["--", "--foo"],
                [["--", "--foo"]],
                [],
            ),
        ]
    )
    async def test_test_execution_component(
        self,
        _unused_name: str,
        flag_list: list[str],
        expected_present_flag_lists: list[list[str]],
        expected_not_present_flags: list[str],
    ) -> None:
        """Test the usage of the TestExecution wrapper on a component test"""

        exec_env = _make_exec_env("/fuchsia", "/out/fuchsia")

        test = execution.TestExecution(
            test_list_file.Test(
                tests_json_file.TestEntry(
                    tests_json_file.TestSection("foo", "//foo", "fuchsia")
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
            args.parse_args(["--no-use-package-hash"] + flag_list),
        )

        command_line = test.command_line()
        self.assertContainsSublist(
            [
                "fx",
                "--dir",
                "/out/fuchsia",
                "ffx",
                "test",
                "run",
                "--realm",
                "foo_tests",
            ],
            command_line,
        )
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

    @parameterized.expand(
        [
            (
                "test execution does not pass a --parallel by default",
                None,
                None,
                [],
            ),
            (
                "test execution respects parallel overrides in test specs",
                1,
                None,
                ["--parallel", "1"],
            ),
            (
                "test execution flag overrides test spec",
                4,
                2,
                ["--parallel", "2"],
            ),
        ]
    )
    async def test_test_execution_parallel_cases(
        self,
        _unused_name: str,
        spec_parallel: int | None,
        flag_parallel: int | None,
        expected_args: list[str],
    ) -> None:
        """Test the usage of the TestExecution wrapper on a component test"""
        exec_env = _make_exec_env("/fuchsia", "/out/fuchsia")

        test = execution.TestExecution(
            test_list_file.Test(
                tests_json_file.TestEntry(
                    tests_json_file.TestSection(
                        "foo", "//foo", "fuchsia", parallel=spec_parallel
                    )
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
            args.parse_args(
                ["--no-use-package-hash"]
                + (
                    []
                    if not flag_parallel
                    else ["--parallel-cases", str(flag_parallel)]
                )
            ),
        )

        self.assertListEqual(
            test.command_line(),
            [
                "fx",
                "--dir",
                "/out/fuchsia",
                "ffx",
                "test",
                "run",
                "--realm",
                "foo_tests",
                "--max-severity-logs",
                "INFO",
                "--min-severity-logs",
                "TRACE",
            ]
            + expected_args
            + [
                "fuchsia-pkg://fuchsia.com/foo#meta/foo_test.cm",
            ],
        )

        self.assertFalse(test.is_hermetic())
        self.assertIsNone(test.environment())
        self.assertTrue(test.should_symbolize())

    @parameterized.expand(
        [
            (
                "test execution does not pass a --no-exception-channel by default",
                None,
                [],
            ),
            (
                "test execution respects create_no_exception_channel in test specs",
                True,
                ["--no-exception-channel"],
            ),
        ]
    )
    async def test_test_execution_create_no_exception_channel(
        self,
        _unused_name: str,
        spec_create_no_exception_channel: bool | None,
        expected_args: list[str],
    ) -> None:
        """Test the usage of the TestExecution wrapper on a component test"""
        exec_env = _make_exec_env("/fuchsia", "/out/fuchsia")

        test = execution.TestExecution(
            test_list_file.Test(
                tests_json_file.TestEntry(
                    tests_json_file.TestSection(
                        "foo",
                        "//foo",
                        "fuchsia",
                        create_no_exception_channel=spec_create_no_exception_channel,
                    )
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
                "--dir",
                "/out/fuchsia",
                "ffx",
                "test",
                "run",
                "--realm",
                "foo_tests",
                "--max-severity-logs",
                "INFO",
                "--min-severity-logs",
                "TRACE",
            ]
            + expected_args
            + [
                "fuchsia-pkg://fuchsia.com/foo#meta/foo_test.cm",
            ],
        )

        self.assertFalse(test.is_hermetic())
        self.assertIsNone(test.environment())
        self.assertTrue(test.should_symbolize())

    async def test_test_execution_component_with_experimental_new_path(
        self,
    ) -> None:
        """Test the usage of the TestExecution wrapper on a component test while using test interface"""

        exec_env = _make_exec_env("/fuchsia", "/out/fuchsia")

        test = execution.TestExecution(
            test_list_file.Test(
                tests_json_file.TestEntry(
                    tests_json_file.TestSection(
                        "foo",
                        "//foo",
                        "fuchsia",
                        new_path="bin/test_component_wrapper.sh",
                    )
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
            args.parse_args(["--use-test-interface", "--", "--some_extra_arg"]),
        )

        command_line = test.command_line()
        self.assertEquals(["./bin/test_component_wrapper.sh"], command_line)
        env = test.environment()
        assert env is not None
        # TODO: Add environment checking when added.
        self.assertDictContainsSubset(
            {
                "CWD": "/out/fuchsia",
                "FUCHSIA_CUSTOM_TEST_ARGS": "--some_extra_arg",
            },
            env,
        )

        # without -use-test-interface-flag
        test = execution.TestExecution(
            test_list_file.Test(
                tests_json_file.TestEntry(
                    tests_json_file.TestSection(
                        "foo",
                        "//foo",
                        "fuchsia",
                        new_path="bin/test_component_wrapper.sh",
                    )
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
            args.parse_args(
                ["--no-use-package-hash", "--", "--some_extra_arg"]
            ),
        )

        command_line = test.command_line()
        self.assertContainsSublist(
            [
                "fx",
                "--dir",
                "/out/fuchsia",
                "ffx",
                "test",
                "run",
                "--realm",
                "foo_tests",
            ],
            command_line,
        )
        self.assertContainsSublist(
            ["fuchsia-pkg://fuchsia.com/foo#meta/foo_test.cm"], command_line
        )

        self.assertContainsSublist(["--", "--some_extra_arg"], command_line)

        self.assertIsNone(test.environment())

    async def test_test_execution_host(self) -> None:
        """Test the usage of the TestExecution wrapper on a host test, and actually run it"""

        with tempfile.TemporaryDirectory() as tmp:
            # We will run ls, but it needs to be relative to the output directory.
            # Find the actual path to the ls binary and symlink it into the
            # output directory.
            ls_path = subprocess.check_output(["which", "ls"]).decode().strip()
            self.assertTrue(os.path.isfile, f"{ls_path} is not a file")
            os.symlink(ls_path, os.path.join(tmp, "ls"))

            exec_env = _make_exec_env("/fuchsia", tmp)

            flags = args.parse_args([])

            test = execution.TestExecution(
                test_list_file.Test(
                    tests_json_file.TestEntry(
                        tests_json_file.TestSection(
                            "foo", "//foo", "linux", path="ls"
                        )
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

            self.assertFalse(
                any([e.error is not None async for e in recorder.iter()])
            )

    @parameterized.expand(
        [
            ("without --e2e enabled", ["--no-e2e"], execution.TestSkipped),
            ("with --e2e enabled", ["--e2e"], execution.TestFailed),
        ]
    )
    @mock.patch("execution.run_command", return_value=None)
    async def test_test_execution_e2e(
        self,
        _unused_name: str,
        extra_flags: list[str],
        expected_exception: type,
        command_mock: mock.MagicMock,
    ) -> None:
        """Test that execution of e2e depends on the --e2e flag"""

        exec_env = _make_exec_env("/fuchsia", "/out/fuchsia")

        flags = args.parse_args(extra_flags)

        test = execution.TestExecution(
            test_list_file.Test(
                tests_json_file.TestEntry(
                    tests_json_file.TestSection(
                        "foo", "//foo", "linux", path="ls"
                    ),
                    environments=[
                        tests_json_file.EnvironmentEntry(
                            dimensions=tests_json_file.DimensionsEntry(
                                device_type="AEMU"
                            )
                        )
                    ],
                ),
                test_list_file.TestListEntry("foo", [], execution=None),
            ),
            exec_env,
            flags,
        )

        self.assertTrue(test._test.is_e2e_test())
        self.assertFalse(test.is_hermetic())
        env = test.environment()
        assert env is not None
        self.assertDictEqual(env, {"CWD": "/out/fuchsia"})
        self.assertFalse(test.should_symbolize())

        recorder = event.EventRecorder()
        recorder.emit_init()

        try:
            await test.run(recorder, flags, event.GLOBAL_RUN_ID)
            self.assertTrue(False, "No exception was raised")
        except Exception as e:
            self.assertIsInstance(e, expected_exception)
        recorder.emit_end()

    @mock.patch("execution.run_command", return_value=None)
    async def test_test_execution_skip_boot_tests(
        self, command_mock: mock.MagicMock
    ) -> None:
        """Test that boot tests are skipped"""

        exec_env = _make_exec_env("/fuchsia", "/out/fuchsia")

        flags = args.parse_args([])

        test = execution.TestExecution(
            test_list_file.Test(
                tests_json_file.TestEntry(
                    tests_json_file.TestSection(
                        "foo", "//foo", "linux", path="ls"
                    ),
                    environments=[
                        tests_json_file.EnvironmentEntry(
                            dimensions=tests_json_file.DimensionsEntry(
                                device_type="AEMU"
                            )
                        )
                    ],
                    product_bundle="some_product",
                ),
                test_list_file.TestListEntry("foo", [], execution=None),
            ),
            exec_env,
            flags,
        )

        self.assertTrue(test._test.is_e2e_test())
        self.assertFalse(test.is_hermetic())
        env = test.environment()
        assert env is not None
        self.assertDictEqual(env, {"CWD": "/out/fuchsia"})
        self.assertFalse(test.should_symbolize())

        recorder = event.EventRecorder()
        recorder.emit_init()

        try:
            await test.run(recorder, flags, event.GLOBAL_RUN_ID)
            self.assertTrue(False, "No exception was raised")
        except Exception as e:
            self.assertIsInstance(e, execution.TestSkipped)
        recorder.emit_end()

    async def test_test_execution_with_package_hash(self) -> None:
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
                        tests_json_file.TestSection(name, "//foo", "linux")
                    ),
                    test_list_file.TestListEntry(
                        name,
                        [],
                        execution=test_list_file.TestListExecutionEntry(name),
                    ),
                )

            def assertTestExecutionFailsUsingMerkleHash(
                error_regex: str,
                test: test_list_file.Test,
                exec_env: environment.ExecutionEnvironment,
            ) -> None:
                self.assertRaisesRegex(
                    execution.TestCouldNotRun,
                    error_regex,
                    lambda: execution.TestExecution(
                        test, exec_env, args.parse_args([])
                    ).command_line(),
                )
                self.assertIsNotNone(
                    execution.TestExecution(
                        test,
                        exec_env,
                        args.parse_args(["--no-use-package-hash"]),
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


class TestExecutionUtils(unittest.IsolatedAsyncioTestCase):
    def _make_command_output(
        self, stdout: str, return_code: int = 0
    ) -> command.CommandOutput:
        return command.CommandOutput(stdout, "", return_code, 0.2, None)

    def setUp(self) -> None:
        self._temp_dir = tempfile.TemporaryDirectory()
        self._env = _make_exec_env(self._temp_dir.name, "")
        return super().setUp()

    def tearDown(self) -> None:
        self._temp_dir.cleanup()
        return super().tearDown()

    @mock.patch("execution.run_command")
    async def test_get_device_environment_success(
        self, command_patch: mock.AsyncMock
    ) -> None:
        command_patch.side_effect = [
            self._make_command_output("127.0.0.1:6000"),
            self._make_command_output("foo-bar"),
            # config get ssh.priv
            self._make_command_output("/foo/path"),
        ]
        device_env = await execution.get_device_environment_from_exec_env(
            self._env
        )
        self.assertDictContainsSubset(
            {
                "address": "127.0.0.1",
                "port": "6000",
                "name": "foo-bar",
                "private_key_path": "/foo/path",
            },
            vars(device_env),
        )

    @mock.patch("execution.run_command")
    async def test_get_device_environment_ipv6(
        self, command_patch: mock.AsyncMock
    ) -> None:
        command_patch.side_effect = [
            self._make_command_output("[::1]:6000"),
            self._make_command_output("foo-bar"),
            # config get ssh.priv
            self._make_command_output("/foo/path"),
        ]
        device_env = await execution.get_device_environment_from_exec_env(
            self._env
        )
        self.assertDictContainsSubset(
            {
                "address": "[::1]",
                "port": "6000",
                "name": "foo-bar",
                "private_key_path": "/foo/path",
            },
            vars(device_env),
        )

    @mock.patch("execution.run_command")
    async def test_get_device_environment_ssh_error(
        self, command_patch: mock.AsyncMock
    ) -> None:
        command_patch.side_effect = [
            self._make_command_output("", return_code=1),
        ]

        try:
            device_env = await execution.get_device_environment_from_exec_env(
                self._env
            )
            self.assertTrue(False, f"Should have failed, got {device_env}")
        except execution.DeviceConfigError as e:
            self.assertRegex(str(e), "Failed to get the ssh address")

    @mock.patch("execution.run_command")
    async def test_get_device_environment_bad_ip_format(
        self, command_patch: mock.AsyncMock
    ) -> None:
        command_patch.side_effect = [
            self._make_command_output("foo"),
        ]

        try:
            device_env = await execution.get_device_environment_from_exec_env(
                self._env
            )
            self.assertTrue(False, f"Should have failed, got {device_env}")
        except execution.DeviceConfigError as e:
            self.assertRegex(str(e), "Could not parse")

    @mock.patch("execution.run_command")
    async def test_get_device_environment_no_target_name(
        self, command_patch: mock.AsyncMock
    ) -> None:
        command_patch.side_effect = [
            self._make_command_output("127.0.0.1:6000"),
            self._make_command_output("", return_code=1),
        ]

        try:
            device_env = await execution.get_device_environment_from_exec_env(
                self._env
            )
            self.assertTrue(False, f"Should have failed, got {device_env}")
        except execution.DeviceConfigError as e:
            self.assertRegex(str(e), "Failed to get the target name")

    async def test_test_execution_gets_env_from_flags(self) -> None:
        """Test TestExecution wrapper uses the environment provided by flags"""

        with tempfile.TemporaryDirectory() as tmp:
            exec_env = _make_exec_env("/fuchsia", tmp)

            flags = args.parse_args(
                ["-e", "foo=bar", "--env", "my_setting=baz"]
            )

            test = execution.TestExecution(
                test_list_file.Test(
                    tests_json_file.TestEntry(
                        tests_json_file.TestSection(
                            "foo", "//foo", "linux", path="wont_exist"
                        )
                    ),
                    test_list_file.TestListEntry("foo", [], execution=None),
                ),
                exec_env,
                flags,
            )

            e = test.environment()
            assert e is not None
            self.assertDictEqual(
                e,
                {"CWD": tmp, "foo": "bar", "my_setting": "baz"},
            )
