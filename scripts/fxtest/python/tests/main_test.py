# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import asyncio
import contextlib
import gzip
import io
import json
import os
import re
import shutil
import signal
import tempfile
import typing
import unittest
import unittest.mock as mock

import async_utils.command as command
from parameterized import parameterized

import args
import environment
import event
import log
import main
import test_list_file

WARNING_LEVEL = event.MessageLevel.WARNING


class TestMainIntegration(unittest.IsolatedAsyncioTestCase):
    """Integration tests for the main entrypoint.

    These tests encapsulate several real-world invocations of fx test,
    with mocked dependencies.
    """

    DEVICE_TESTS_IN_INPUT = 1
    HOST_TESTS_IN_INPUT = 3
    E2E_TESTS_IN_INPUT = 1
    TOTAL_TESTS_IN_INPUT = DEVICE_TESTS_IN_INPUT + HOST_TESTS_IN_INPUT
    TOTAL_NON_E2E_TESTS_IN_INPUT = TOTAL_TESTS_IN_INPUT - E2E_TESTS_IN_INPUT

    def setUp(self) -> None:
        # Set up a Fake fuchsia directory.
        self.fuchsia_dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.fuchsia_dir.cleanup)

        # Set up mocks
        self.mocks = []

        # Retain the real build dir, if one exists.
        real_fuchsia_dir = os.getenv("FUCHSIA_DIR")

        # Intercept environment and instantiate a new mock FUCHSIA_DIR.
        self.mocks.append(
            mock.patch(
                "os.environ",
                {"FUCHSIA_DIR": self.fuchsia_dir.name},
            )
        )
        for m in self.mocks:
            m.start()
            self.addCleanup(m.stop)

        # Correct for location of the test data files between coverage.py
        # script and how tests are run in-tree.
        cur_path = os.path.dirname(__file__)
        while not os.path.isdir(cur_path):
            cur_path = os.path.split(cur_path)[0]

        # We use an external program to handle fuzzy matching called "dldist".
        # Put the program in the correct location so that the main script can
        # find it.
        os.makedirs(os.path.join(self.fuchsia_dir.name, "bin"))
        dldist_path = os.path.join(self.fuchsia_dir.name, "bin", "dldist")
        if os.path.exists(os.path.join(cur_path, "bin", "dldist")):
            # This path is used when executing the python_host_test target.
            print("Using the local dldist for matching")
            shutil.copy(os.path.join(cur_path, "bin", "dldist"), dldist_path)
        else:
            # This path is used when running coverage.py.
            print("Trying to use FUCHSIA_DIR dldist for the test")
            assert real_fuchsia_dir is not None
            build_dir: str
            with open(os.path.join(real_fuchsia_dir, ".fx-build-dir")) as f:
                build_dir = os.path.join(real_fuchsia_dir, f.read().strip())
            print(build_dir)
            assert os.path.isdir(build_dir)
            shutil.copy(
                os.path.join(build_dir, "host-tools", "dldist"), dldist_path
            )

        self.test_data_path = os.path.join(cur_path, "test_data/build_output")

        self.assertTrue(
            os.path.isfile(os.path.join(self.test_data_path, "tests.json")),
            f"path was {self.test_data_path} for {__file__}",
        )
        self.assertTrue(
            os.path.isfile(os.path.join(self.test_data_path, "test-list.json")),
            f"path was {self.test_data_path} for {__file__}",
        )
        self.test_list_input = os.path.join(
            self.test_data_path, "test-list.json"
        )
        self.assertTrue(
            os.path.isfile(
                os.path.join(self.test_data_path, "package-repositories.json")
            ),
            f"path was {self.test_data_path} for {__file__}",
        )

        with open(
            os.path.join(self.fuchsia_dir.name, ".fx-build-dir"), "w"
        ) as f:
            f.write("out/default")

        self.out_dir = os.path.join(self.fuchsia_dir.name, "out/default")
        os.makedirs(self.out_dir)

        for name in [
            "tests.json",
            "test-list.json",
            "package-repositories.json",
            "package-targets.json",
            "all_package_manifests.list",
        ]:
            shutil.copy(
                os.path.join(self.test_data_path, name),
                os.path.join(self.out_dir, name),
            )

        # Simulate the generated package metadata to test merging.
        gen_dir = os.path.join(
            self.out_dir, "gen", "build", "images", "updates"
        )
        os.makedirs(gen_dir)
        with open(
            os.path.join(
                gen_dir, "package_manifests_from_metadata.list.package_metadata"
            ),
            "w",
        ) as f:
            f.writelines(
                [
                    "obj/foo/package_manifest.json",
                    "obj/bar/package_manifest.json",
                    "obj/baz/package_manifest.json",
                ]
            )

        self._mock_get_device_environment(
            environment.DeviceEnvironment(
                "localhost", "8080", "foo", "/foo.key"
            )
        )

        # Make sure that generated test list is identical to input.
        self._mock_generate_test_list()

        return super().setUp()

    def _mock_run_commands_in_parallel(self, stdout: str) -> mock.MagicMock:
        m = mock.AsyncMock(
            return_value=[mock.MagicMock(stdout=stdout, return_code=0)]
        )
        patch = mock.patch("main.run_commands_in_parallel", m)
        patch.start()
        self.addCleanup(patch.stop)
        return m

    def _mock_run_command(
        self,
        return_code: int,
        async_handler: (
            typing.Callable[[typing.Any, typing.Any], typing.Awaitable[None]]
            | None
        ) = None,
    ) -> mock.MagicMock:
        async def handler(
            *args: typing.Any, **kwargs: typing.Any
        ) -> typing.Any:
            if async_handler is not None:
                await async_handler(*args, **kwargs)
            return mock.MagicMock(
                return_code=return_code, stdout="", stderr="", was_timeout=False
            )

        m = mock.AsyncMock(side_effect=handler)
        patch = mock.patch("main.execution.run_command", m)
        patch.start()
        self.addCleanup(patch.stop)
        return m

    def _mock_generate_test_list(self) -> mock.MagicMock:
        test_list_entries = test_list_file.TestListFile.entries_from_file(
            self.test_list_input
        )
        m = mock.AsyncMock(return_value=test_list_entries)
        patch = mock.patch("main.AsyncMain._generate_test_list", m)
        patch.start()
        self.addCleanup(patch.stop)
        return m

    def _mock_subprocess_call(self, value: int) -> mock.MagicMock:
        m = mock.MagicMock(return_value=value)
        patch = mock.patch("main.subprocess.call", m)
        patch.start()
        self.addCleanup(patch.stop)
        return m

    def _mock_has_device_connected(self, value: bool) -> None:
        m = mock.AsyncMock(return_value=value)
        patch = mock.patch("main.has_device_connected", m)
        patch.start()
        self.addCleanup(patch.stop)

    def _mock_has_tests_in_base(self, test_packages: list[str]) -> None:
        with open(os.path.join(self.out_dir, "base_packages.list"), "w") as f:
            json.dump(
                {
                    "content": {
                        "manifests": test_packages,
                    }
                },
                f,
            )

    def _make_call_args_prefix_set(
        self, call_list: mock._CallList
    ) -> set[tuple[str, ...]]:
        """Given a list of mock calls, turn them into a set of prefixes for comparison.

        For instance, if the mock call is ("fx", "run", "command") the output
        is: {
            ('fx',),
            ('fx', 'run'),
            ('fx', 'run', 'command'),
        }

        This can be used to check containment.

        Args:
            call_list (mock._CallList): Calls to process.

        Returns:
            set[list[typing.Any]]: Set of prefixes to calls.
        """
        ret: set[tuple[str, ...]] = set()
        for call in call_list:
            args, _ = call
            cur = []
            if args and isinstance(args[0], list):
                # Correct for subprocess.call using lists and not *args.
                args = args[0]
            for a in args:
                cur.append(a)
                ret.add(tuple(cur))

        return ret

    def _mock_get_device_environment(
        self, env: environment.DeviceEnvironment
    ) -> mock.MagicMock:
        m = mock.AsyncMock(return_value=env)
        patch = mock.patch(
            "main.execution.get_device_environment_from_exec_env", m
        )
        patch.start()
        self.addCleanup(patch.stop)
        return m

    def assertIsSubset(
        self, subset: set[typing.Any], full: set[typing.Any]
    ) -> None:
        inter = full.intersection(subset)
        self.assertEqual(
            inter, subset, f"Full set was\n {self.prettyFormatPrefixes(full)}"
        )

    def prettyFormatPrefixes(self, vals: set[typing.Any]) -> str:
        return "\n ".join(map(lambda x: " ".join(x), sorted(vals)))

    async def test_dry_run(self) -> None:
        """Test a basic dry run of the command."""
        recorder = event.EventRecorder()
        ret = await main.async_main_wrapper(
            args.parse_args(["--simple", "--dry"]), recorder=recorder
        )
        self.assertEqual(ret, 0)

        selection_events: list[event.TestSelectionPayload] = [
            e.payload.test_selections
            async for e in recorder.iter()
            if e.payload is not None and e.payload.test_selections is not None
        ]

        self.assertEqual(len(selection_events), 1)
        selection_event = selection_events[0]
        self.assertEqual(
            len(selection_event.selected), self.TOTAL_TESTS_IN_INPUT
        )

    async def test_fuzzy_dry_run(self) -> None:
        """Test a dry run of the command for fuzzy matching"""
        recorder = event.EventRecorder()
        ret = await main.async_main_wrapper(
            args.parse_args(["--simple", "--dry", "--fuzzy=1", "foo_test"]),
            recorder=recorder,
        )
        self.assertEqual(ret, 0)

        selection_events: list[event.TestSelectionPayload] = [
            e.payload.test_selections
            async for e in recorder.iter()
            if e.payload is not None and e.payload.test_selections is not None
        ]

        self.assertEqual(len(selection_events), 1)
        selection_event = selection_events[0]
        self.assertEqual(len(selection_event.selected), 1)

    async def test_cancel_before_tests_run(self) -> None:
        """Test that SIGINT before tests start running immediately stops execution"""

        self._mock_run_command(0)
        self._mock_has_device_connected(True)
        self._mock_has_tests_in_base([])

        ready_to_kill = asyncio.Event()

        # Make builds hang for a long time, signalling that we should
        # trigger a SIGINT at the point the build starts.
        async def build_handler(_: typing.Any) -> bool:
            ready_to_kill.set()
            await asyncio.sleep(3600)
            return False

        build_patch = mock.patch(
            "main.AsyncMain._do_build",
            mock.AsyncMock(side_effect=build_handler),
        )
        build_patch.start()
        self.addCleanup(build_patch.stop)

        recorder = event.EventRecorder()
        main_task = asyncio.Task(
            main.async_main_wrapper(
                args.parse_args(["--simple"]), recorder=recorder
            )
        )

        await ready_to_kill.wait()
        os.kill(os.getpid(), signal.SIGINT)

        ret = await main_task
        self.assertEqual(ret, 1)
        errors = {e.error async for e in recorder.iter() if e.error is not None}
        self.assertIsSubset({"Terminated due to interrupt"}, errors)

    async def test_cancel_tests_wraps_up(self) -> None:
        """Test that SIGINT while tests are running allows them to wrap up and prints output"""

        ready_to_kill = asyncio.Event()

        async def command_handler(
            *args: typing.Any, **kwargs: typing.Any
        ) -> None:
            event: asyncio.Event = kwargs.get("abort_signal")  # type: ignore
            assert event is not None
            ready_to_kill.set()
            await event.wait()

        _command_mock = self._mock_run_command(
            15, async_handler=command_handler
        )
        self._mock_has_device_connected(True)
        self._mock_has_tests_in_base([])

        recorder = event.EventRecorder()
        main_task = asyncio.Task(
            main.async_main_wrapper(
                args.parse_args(["--simple", "--no-build"]), recorder=recorder
            )
        )

        await ready_to_kill.wait()
        os.kill(os.getpid(), signal.SIGINT)

        ret = await main_task
        self.assertEqual(ret, 1)
        errors = {e.error async for e in recorder.iter() if e.error is not None}
        self.assertIsSubset({"Failed to run all tests"}, errors)

        aborted_cases = {
            (payload_event.status, payload_event.message)
            async for e in recorder.iter()
            if (payload := e.payload) is not None
            and (payload_event := payload.test_suite_ended) is not None
        }
        self.assertSetEqual(
            aborted_cases,
            {
                (
                    event.TestSuiteStatus.ABORTED,
                    "Test suite aborted due to user interrupt.",
                )
            },
        )

    async def test_double_sigint_cancels_everything(self) -> None:
        """Test that sending SIGINT twice cancels all tasks, no matter how long running"""

        ready_to_kill = asyncio.Event()

        async def command_handler(
            *args: typing.Any, **kwargs: typing.Any
        ) -> None:
            ready_to_kill.set()
            await asyncio.sleep(3600)

        _command_mock = self._mock_run_command(
            15, async_handler=command_handler
        )
        self._mock_has_device_connected(True)
        self._mock_has_tests_in_base([])

        recorder = event.EventRecorder()
        main_task = asyncio.Task(
            main.async_main_wrapper(
                args.parse_args(["--simple", "--no-build"]), recorder=recorder
            )
        )

        await ready_to_kill.wait()
        os.kill(os.getpid(), signal.SIGINT)
        await asyncio.sleep(0.5)
        os.kill(os.getpid(), signal.SIGINT)

        ret = await main_task
        self.assertEqual(ret, 1)

    @parameterized.expand(
        [
            (["--host"], HOST_TESTS_IN_INPUT - E2E_TESTS_IN_INPUT),
            (["--device"], DEVICE_TESTS_IN_INPUT),
            (["--only-e2e"], E2E_TESTS_IN_INPUT),
            # TODO(https://fxbug.dev/338667899): Enable when we determine how to handle opt-in e2e.
            # ([], TOTAL_NON_E2E_TESTS_IN_INPUT),
            (["--e2e"], TOTAL_TESTS_IN_INPUT),
        ]
    )
    async def test_selection_flags(
        self, extra_flags: list[str], expected_count: int
    ) -> None:
        """Test that the correct --device, --host, or --e2e tests are selected"""

        recorder = event.EventRecorder()
        ret = await main.async_main_wrapper(
            args.parse_args(["--simple", "--dry"] + extra_flags),
            recorder=recorder,
        )
        self.assertEqual(ret, 0)

        selection_events: list[event.TestSelectionPayload] = [
            e.payload.test_selections
            async for e in recorder.iter()
            if e.payload is not None and e.payload.test_selections is not None
        ]

        self.assertEqual(len(selection_events), 1)
        selection_event = selection_events[0]
        self.assertEqual(len(selection_event.selected), expected_count)

    @parameterized.expand(
        [
            ("--use-package-hash", DEVICE_TESTS_IN_INPUT),
            ("--no-use-package-hash", 0),
        ]
    )
    async def test_use_package_hash(
        self, flag_name: str, expected_hash_matches: int
    ) -> None:
        """Test ?hash= is used only when --use-package-hash is set"""

        command_mock = self._mock_run_command(0)
        self._mock_has_device_connected(True)
        self._mock_has_tests_in_base([])

        ret = await main.async_main_wrapper(
            args.parse_args(["--simple", "--no-build"] + [flag_name])
        )
        self.assertEqual(ret, 0)

        call_prefixes = self._make_call_args_prefix_set(
            command_mock.call_args_list
        )

        self.assertIsSubset(
            {
                (
                    "fx",
                    "--dir",
                    self.out_dir,
                    "ffx",
                    "test",
                    "run",
                ),
            },
            call_prefixes,
        )

        hash_params_found: int = 0
        for prefix_list in call_prefixes:
            entry: str
            for entry in prefix_list:
                if "?hash=" in entry:
                    hash_params_found += 1

        self.assertEqual(
            hash_params_found,
            expected_hash_matches,
            f"Prefixes were\n{self.prettyFormatPrefixes(call_prefixes)}",
        )

    @parameterized.expand(
        [
            ("default suggestions", [], 6),
            ("custom suggestion count", ["--suggestion-count=10"], 10),
            ("suppress suggestions", ["--no-show-suggestions"], 0),
        ]
    )
    async def test_suggestions(
        self,
        _unused_name: str,
        extra_flags: list[str],
        expected_suggestion_count: int,
    ) -> None:
        """Test that targets are suggested when there are no test matches."""
        mocked_commands = self._mock_run_commands_in_parallel("No matches")
        ret = await main.async_main_wrapper(
            args.parse_args(
                ["--simple", "non_existent_test_does_not_match"] + extra_flags
            )
        )
        self.assertEqual(ret, 1)
        if expected_suggestion_count > 0:
            self.assertListEqual(
                mocked_commands.call_args[0][0],
                [
                    [
                        "fx",
                        "--dir",
                        self.out_dir,
                        "search-tests",
                        f"--max-results={expected_suggestion_count}",
                        "--no-color",
                        "non_existent_test_does_not_match",
                    ]
                ],
            )
        else:
            self.assertListEqual(mocked_commands.call_args_list, [])

        # TODO(b/295340412): Test that suggestions are suppressed.

    async def test_full_success(self) -> None:
        """Test that we can run all tests and report success"""

        command_mock = self._mock_run_command(0)
        subprocess_mock = self._mock_subprocess_call(0)
        self._mock_has_device_connected(True)
        self._mock_has_tests_in_base([])

        ret = await main.async_main_wrapper(args.parse_args(["--simple"]))
        self.assertEqual(ret, 0)

        call_prefixes = self._make_call_args_prefix_set(
            command_mock.call_args_list
        )
        call_prefixes.update(
            self._make_call_args_prefix_set(subprocess_mock.call_args_list)
        )

        # Make sure we built, published, and ran the device test.
        self.assertIsSubset(
            {
                (
                    "fx",
                    "--dir",
                    self.out_dir,
                    "build",
                    "--default",
                    "//src/sys:foo_test_package",
                    "--toolchain=//build/toolchain/host:x64",
                    "//src/sys:bar_test",
                    "//src/sys:baz_test",
                    "//src/tests/end_to_end:example_e2e_test",
                    "--default",
                    "updates",
                ),
                (
                    "fx",
                    "--dir",
                    self.out_dir,
                    "ffx",
                    "repository",
                    "publish",
                ),
                (
                    "fx",
                    "--dir",
                    self.out_dir,
                    "ffx",
                    "test",
                    "run",
                ),
            },
            call_prefixes,
        )

        # Make sure we ran the host tests.
        self.assertTrue(any(["bar_test" in v[0] for v in call_prefixes]))
        self.assertTrue(any(["baz_test" in v[0] for v in call_prefixes]))

        # Try running again, but this time replay the previous execution.
        output = mock.MagicMock(wraps=io.StringIO())
        output.fileno = lambda: -1
        with contextlib.redirect_stdout(output):
            ret = await main.async_main_wrapper(
                args.parse_args(
                    [
                        "--simple",
                        "-q",
                        "--previous",
                        "replay",
                        "--replay-speed",
                        "5",
                    ]
                ),
                replay_mode=True,
            )
            self.assertEqual(0, ret)

        contents = list(map(str.strip, output.getvalue().split("\n")))
        contents_for_printing = "\n ".join(contents)
        self.assertTrue(
            {
                "PASSED: host_x64/bar_test",
                "PASSED: fuchsia-pkg://fuchsia.com/foo-test#meta/foo_test.cm",
                "PASSED: host_x64/baz_test",
                "SKIPPED: host_x64/example_e2e_test",
            }.issubset(set(contents)),
            f"Contents were:\n {contents_for_printing}",
        )

    async def test_build_e2e(self) -> None:
        """Test that we build an updates package for e2e tests"""

        command_mock = self._mock_run_command(0)
        subprocess_mock = self._mock_subprocess_call(0)
        self._mock_has_device_connected(True)
        self._mock_has_tests_in_base([])

        ret = await main.async_main_wrapper(
            args.parse_args(["--simple", "--only-e2e"])
        )
        self.assertEqual(ret, 0)

        call_prefixes = self._make_call_args_prefix_set(
            command_mock.call_args_list
        )
        call_prefixes.update(
            self._make_call_args_prefix_set(subprocess_mock.call_args_list)
        )

        # Make sure we built, published, and ran the device test.
        self.assertIsSubset(
            {
                (
                    "fx",
                    "--dir",
                    os.path.join(self.fuchsia_dir.name, "out/default"),
                    "build",
                    "--toolchain=//build/toolchain/host:x64",
                    "//src/tests/end_to_end:example_e2e_test",
                    "--default",
                    "updates",
                ),
                (
                    "fx",
                    "--dir",
                    self.out_dir,
                    "ffx",
                    "repository",
                    "publish",
                ),
            },
            call_prefixes,
        )

        # Make sure we ran the host tests.
        self.assertTrue(
            any(["example_e2e_test" in v[0] for v in call_prefixes])
        )

    async def test_no_build(self) -> None:
        """Test that we can run all tests and report success"""

        command_mock = self._mock_run_command(0)
        self._mock_has_device_connected(True)
        self._mock_has_tests_in_base([])

        ret = await main.async_main_wrapper(
            args.parse_args(["--simple", "--no-build"])
        )
        self.assertEqual(ret, 0)

        call_prefixes = self._make_call_args_prefix_set(
            command_mock.call_args_list
        )

        self.assertFalse(
            ("fx", "--dir", self.out_dir, "build") in call_prefixes
        )
        self.assertFalse(
            ("fx", "--dir", self.out_dir, "ffx", "repository", "publish")
            in call_prefixes
        )

        self.assertIsSubset(
            {
                ("fx", "--dir", self.out_dir, "ffx", "test", "run"),
            },
            call_prefixes,
        )

        # Make sure we ran the host test.
        self.assertTrue(any(["bar_test" in v[0] for v in call_prefixes]))
        self.assertTrue(any(["baz_test" in v[0] for v in call_prefixes]))

    async def test_first_failure(self) -> None:
        """Test that one failing test aborts the rest with --fail"""

        command_mock = self._mock_run_command(1)
        command_mock.side_effect = [
            command.CommandOutput("out", "err", 1, 10, None),
            command.CommandOutput("out", "err", 1, 10, None),
            command.CommandOutput("out", "err", 1, 10, None),
        ]

        self._mock_has_device_connected(True)
        self._mock_has_tests_in_base([])

        ret = await main.async_main_wrapper(
            args.parse_args(["--simple", "--no-build", "--fail"])
        )

        # bar_test and baz_test are not hermetic, so cannot run at the same time.
        # One of them will run before the other, which means --fail
        # prevents one of them from starting, and we expect to see
        # only bar_test (since baz_test is defined later in the file)
        call_prefixes = self._make_call_args_prefix_set(
            command_mock.call_args_list
        )
        self.assertEqual(ret, 1)

        self.assertTrue(any(["bar_test" in v[0] for v in call_prefixes]))
        self.assertFalse(any(["baz_test" in v[0] for v in call_prefixes]))

    async def test_count(self) -> None:
        """Test that we can re-run a test multiple times with --count"""

        command_mock = self._mock_run_command(0)

        self._mock_has_device_connected(True)
        self._mock_has_tests_in_base([])

        # Run each test 3 times, no parallel to better match behavior of failure case test.
        ret = await main.async_main_wrapper(
            args.parse_args(
                ["--simple", "--no-build", "--count=3", "--parallel=1"]
            )
        )
        self.assertEqual(ret, 0)

        self.assertEqual(
            3,
            sum(["bar_test" in v[0][0] for v in command_mock.call_args_list]),
            command_mock.call_args_list,
        )
        self.assertEqual(
            3,
            sum(["baz_test" in v[0][0] for v in command_mock.call_args_list]),
            command_mock.call_args_list,
        )
        self.assertEqual(
            3,
            sum(
                [
                    "foo-test?hash=" in " ".join(v[0])
                    for v in command_mock.call_args_list
                ]
            ),
            command_mock.call_args_list,
        )

    async def test_count_with_timeout(self) -> None:
        """Test that we abort running the rest of the tests in a --count group if a timeout occurs."""

        command_mock = self._mock_run_command(1)
        command_mock.return_value = command.CommandOutput(
            "", "", 1, 10, None, was_timeout=True
        )

        self._mock_has_device_connected(True)
        self._mock_has_tests_in_base([])

        # Run each test 3 times, no parallel to better match behavior of failure case test.
        ret = await main.async_main_wrapper(
            args.parse_args(
                [
                    "--simple",
                    "--no-build",
                    "--count=3",
                    "--parallel=1",
                ]
            )
        )
        self.assertEqual(ret, 1)

        self.assertEqual(
            1,
            sum(["bar_test" in v[0][0] for v in command_mock.call_args_list]),
            command_mock.call_args_list,
        )
        self.assertEqual(
            1,
            sum(["baz_test" in v[0][0] for v in command_mock.call_args_list]),
            command_mock.call_args_list,
        )
        self.assertEqual(
            1,
            sum(
                [
                    "foo-test?hash=" in " ".join(v[0])
                    for v in command_mock.call_args_list
                ]
            ),
            command_mock.call_args_list,
        )

    async def test_list_command(self) -> None:
        """Test that we can list test cases using --list"""

        command_mock = self._mock_run_commands_in_parallel(
            "foo::test\nbar::test",
        )

        self._mock_has_device_connected(True)
        self._mock_has_tests_in_base([])

        recorder = event.EventRecorder()

        # This only works if the first test is a device test.
        ret = await main.async_main_wrapper(
            args.parse_args(["--simple", "--no-build", "--list", "--limit=1"]),
            recorder=recorder,
        )
        self.assertEqual(ret, 0)
        self.assertEqual(command_mock.call_count, 1)

        events = [
            e.payload.enumerate_test_cases
            async for e in recorder.iter()
            if e.payload is not None
            and e.payload.enumerate_test_cases is not None
        ]
        self.assertEqual(len(events), 1)

        self.assertEqual(
            events[0].test_case_names,
            [
                "foo::test",
                "bar::test",
            ],
        )

    @mock.patch("main.run_build_with_suspended_output", side_effect=[0])
    async def test_updateifinbase(self, _build_mock: mock.AsyncMock) -> None:
        """Test that we appropriately update tests in base"""

        command_mock = self._mock_run_command(0)

        self._mock_has_device_connected(True)
        self._mock_has_tests_in_base(["foo-test"])

        ret = await main.async_main_wrapper(
            args.parse_args(
                [
                    "--simple",
                    "--no-build",
                    "--updateifinbase",
                    "--parallel",
                    "1",
                ]
            )
        )
        self.assertEqual(ret, 0)
        call_prefixes = self._make_call_args_prefix_set(
            command_mock.call_args_list
        )
        self.assertIsSubset(
            {
                (
                    "fx",
                    "--dir",
                    self.out_dir,
                    "ota",
                    "--no-build",
                )
            },
            call_prefixes,
        )

    async def test_print_logs_success(self) -> None:
        """Test that print_logs searches for logs, can be given a log,
        and handles invalid data
        """
        env = environment.ExecutionEnvironment.initialize_from_args(
            args.parse_args([])
        )
        assert env.log_file
        # Create a sample log with 3 tests running
        recorder = event.EventRecorder()
        recorder.emit_init()

        # Simulate one test suite
        test_id = recorder.emit_test_suite_started("foo", hermetic=False)
        program_id = recorder.emit_program_start(
            "bar", ["abcd"], parent=test_id
        )
        recorder.emit_program_output(
            program_id, "Data", event.ProgramOutputStream.STDOUT
        )
        recorder.emit_program_termination(program_id, 0)
        recorder.emit_test_suite_ended(
            test_id,
            event.TestSuiteStatus.PASSED,
            message=None,
        )

        test_2 = recorder.emit_test_suite_started(
            "//other:test2", hermetic=True
        )
        test_3 = recorder.emit_test_suite_started(
            "//other:test3", hermetic=True
        )
        program_2 = recorder.emit_program_start(
            "test", ["arg", "1"], parent=test_2
        )
        program_3 = recorder.emit_program_start(
            "test", ["arg", "2"], parent=test_3
        )
        recorder.emit_program_output(
            program_3,
            "line for test 3",
            stream=event.ProgramOutputStream.STDOUT,
        )
        recorder.emit_program_output(
            program_2,
            "line for test 2",
            stream=event.ProgramOutputStream.STDOUT,
        )
        recorder.emit_program_termination(program_2, 0)
        recorder.emit_program_termination(program_3, 0)
        recorder.emit_test_suite_ended(
            test_2, event.TestSuiteStatus.FAILED, message=None
        )
        recorder.emit_test_suite_ended(
            test_3, event.TestSuiteStatus.PASSED, message=None
        )
        recorder.emit_end()

        with gzip.open(env.log_file, "wt") as out_file:
            async for e in recorder.iter():
                json.dump(e.to_dict(), out_file)  # type:ignore[attr-defined]
                print("", file=out_file)

        def assert_print_logs_output(return_code: int, output: str) -> None:
            self.assertEqual(return_code, 0, f"Content was:\n{output}")
            self.assertIsNotNone(
                re.search(r"3 tests were run", output, re.MULTILINE),
                f"Did not find substring, content was:\n{output}",
            )

        # Test finding most recent log file.
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            return_code = main.do_print_logs(args.parse_args([]))
            assert_print_logs_output(return_code, output.getvalue())

        # Test finding specific log file.
        output = io.StringIO()
        new_file_path = os.path.join(env.out_dir, "other-file.json.gz")
        shutil.move(env.log_file, new_file_path)

        with contextlib.redirect_stdout(output):
            return_code = main.do_print_logs(
                args.parse_args(["--logpath", new_file_path])
            )
            self.assertEqual(
                return_code, 0, f"Content was:\n{output.getvalue()}"
            )
            self.assertIsNotNone(
                re.search(r"3 tests were run", output.getvalue(), re.MULTILINE),
                f"Did not find substring, content was:\n{output.getvalue()}",
            )

    async def test_print_logs_failure(self) -> None:
        """Test that --print-logs prints an error and exits if the log cannot be found"""

        # Default search location
        output = io.StringIO()
        with contextlib.redirect_stderr(output):
            self.assertEqual(main.do_print_logs(args.parse_args([])), 1)
            self.assertIsNotNone(
                re.search(r"No log files found", output.getvalue()),
                f"Did not find substring, output was:\n{output.getvalue()}",
            )

        # Specific missing file
        output = io.StringIO()
        with contextlib.redirect_stderr(output):
            with tempfile.TemporaryDirectory() as td:
                path = os.path.join(td, "does-not-exist")
                self.assertEqual(
                    main.do_print_logs(args.parse_args(["--logpath", path])),
                    1,
                )
                self.assertIsNotNone(
                    re.search(r"No log files found", output.getvalue()),
                    f"Did not find substring, output was:\n{output.getvalue()}",
                )

        # Specific file is not a gzip file
        output = io.StringIO()
        with contextlib.redirect_stderr(output):
            with tempfile.TemporaryDirectory() as td:
                path = os.path.join(td, "does-not-exist")
                with open(path, "w") as f:
                    f.writelines(["hello world"])
                self.assertEqual(
                    main.do_print_logs(args.parse_args(["--logpath", path])),
                    1,
                )
                self.assertIsNotNone(
                    re.search(
                        r"File does not appear to be a gzip file",
                        output.getvalue(),
                    ),
                    f"Did not find substring, output was:\n{output.getvalue()}",
                )

    @mock.patch("main.termout.is_valid", return_value=False)
    async def test_log_to_stdout(self, _termout_mock: mock.Mock) -> None:
        """Test that we can log everything to stdout, and it parses as JSON lines"""

        self._mock_run_command(0)
        self._mock_subprocess_call(0)
        self._mock_has_device_connected(True)
        self._mock_has_tests_in_base([])

        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            ret = await main.async_main_wrapper(
                args.parse_args(["--logpath", "-"])
            )
            self.assertEqual(ret, 0)
        for line in output.getvalue().splitlines():
            if not line:
                continue
            try:
                json.loads(line)
            except json.JSONDecodeError as e:
                self.fail(
                    f"Failed to parse line as JSON.\nLine: {line}\nError: {e}"
                )

    async def test_artifact_options(self) -> None:
        """Test that we handle artifact output directories and can query their value"""

        self._mock_run_command(0)
        self._mock_subprocess_call(0)
        self._mock_has_device_connected(True)
        self._mock_has_tests_in_base([])

        with self.subTest("no artifact path still produces empty event"):
            with tempfile.TemporaryDirectory() as td:
                logpath = os.path.join(td, "log.json.gz")
                flags = args.parse_args(["--simple", "--logpath", logpath])
                ret = await main.async_main_wrapper(flags)
                self.assertEqual(ret, 0)

                env = environment.ExecutionEnvironment.initialize_from_args(
                    flags, create_log_file=False
                )

                artifact_path: str | None = None
                for log_entry in log.LogSource.from_env(env).read_log():
                    if (event := log_entry.log_event) is not None:
                        if (
                            event.payload is not None
                            and (path := event.payload.artifact_directory_path)
                            is not None
                        ):
                            artifact_path = path
                    self.assertIsNone(log_entry.error)
                    self.assertIsNone(log_entry.warning)
                self.assertEqual(artifact_path, "")

                # Using the output log file, we should get an error requesting the artifact output.
                stderr = io.StringIO()
                with contextlib.redirect_stderr(stderr):
                    ret = main.do_process_previous(
                        args.parse_args(
                            [
                                "-pr",
                                "artifact-path",
                                "--logpath",
                                logpath,
                            ]
                        )
                    )
                    self.assertNotEqual(ret, 0)

                lines = stderr.getvalue().splitlines()
                self.assertEqual(
                    lines,
                    [
                        "ERROR: The previous run did not specify --artifact-output-directory. Run again with that flag set to get the path."
                    ],
                )

        with self.subTest(
            "artifact path by default goes to the top level directory"
        ):
            with tempfile.TemporaryDirectory() as td:
                logpath = os.path.join("log.json.gz")
                artifact_root = os.path.join(td, "artifacts")
                flags = args.parse_args(
                    [
                        "--simple",
                        "--logpath",
                        logpath,
                        "--outdir",
                        artifact_root,
                    ]
                )
                ret = await main.async_main_wrapper(flags)
                self.assertEqual(ret, 0)

                env = environment.ExecutionEnvironment.initialize_from_args(
                    flags, create_log_file=False
                )

                artifact_path = None
                for log_entry in log.LogSource.from_env(env).read_log():
                    if (event := log_entry.log_event) is not None:
                        if (
                            event.payload is not None
                            and (path := event.payload.artifact_directory_path)
                            is not None
                        ):
                            artifact_path = path
                    self.assertIsNone(log_entry.error)
                    self.assertIsNone(log_entry.warning)
                self.assertEqual(artifact_path, artifact_root)

                # Path gets created automatically.
                self.assertTrue(os.path.isdir(artifact_root))

                # Delete the artifact directory, checking what happens when it does not exist.
                shutil.rmtree(artifact_root)

                # Using the output log file, we should see an error getting the path because the directory will not be present.
                stderr = io.StringIO()
                with contextlib.redirect_stderr(stderr):
                    ret = main.do_process_previous(
                        args.parse_args(
                            [
                                "-pr",
                                "artifact-path",
                                "--logpath",
                                logpath,
                            ]
                        )
                    )
                    self.assertNotEqual(ret, 0)

                lines = stderr.getvalue().splitlines()
                self.assertEqual(
                    lines,
                    [
                        "ERROR: The artifact directory is missing, it may have been deleted."
                    ],
                )

                # Create the artifact directory. Listing artifact path should work now.
                os.makedirs(artifact_root)
                stdout = mock.MagicMock(wraps=io.StringIO())
                stdout.fileno = lambda: -1
                with contextlib.redirect_stdout(stdout):
                    ret = main.do_process_previous(
                        args.parse_args(
                            [
                                "-pr",
                                "artifact-path",
                                "--logpath",
                                logpath,
                            ]
                        )
                    )
                    self.assertEqual(ret, 0)

                lines = stdout.getvalue().splitlines()
                self.assertEqual(
                    lines,
                    [artifact_root],
                )

        with self.subTest(
            "--timestamp-artifacts causes artifacts to go to subdir"
        ):
            with tempfile.TemporaryDirectory() as td:
                logpath = os.path.join("log.json.gz")
                artifact_root = os.path.join(td, "artifacts")
                flags = args.parse_args(
                    [
                        "--simple",
                        "--logpath",
                        logpath,
                        "--outdir",
                        artifact_root,
                        "--timestamp-artifacts",
                    ]
                )
                ret = await main.async_main_wrapper(flags)
                self.assertEqual(ret, 0)

                env = environment.ExecutionEnvironment.initialize_from_args(
                    flags, create_log_file=False
                )

                artifact_path = None
                for log_entry in log.LogSource.from_env(env).read_log():
                    if (event := log_entry.log_event) is not None:
                        if (
                            event.payload is not None
                            and (path := event.payload.artifact_directory_path)
                            is not None
                        ):
                            artifact_path = path
                    self.assertIsNone(log_entry.error)
                    self.assertIsNone(log_entry.warning)
                self.assertNotEqual(artifact_path, artifact_root)
                assert artifact_path is not None
                self.assertEqual(
                    os.path.commonprefix([artifact_path, artifact_root]),
                    artifact_root,
                )

        with self.subTest(
            "it is an error to output to an existing, non-empty directory"
        ):
            with tempfile.TemporaryDirectory() as td:
                logpath = os.path.join("log.json.gz")
                artifact_root = os.path.join(td, "artifacts")
                os.mkdir(artifact_root)
                with open(os.path.join(artifact_root, "some_file"), "w") as f:
                    f.write("Demo data")
                flags = args.parse_args(
                    [
                        "--simple",
                        "--logpath",
                        logpath,
                        "--outdir",
                        artifact_root,
                        "--no-timestamp-artifacts",
                    ]
                )
                ret = await main.async_main_wrapper(flags)
                self.assertEqual(ret, 1)

                env = environment.ExecutionEnvironment.initialize_from_args(
                    flags, create_log_file=False
                )

                artifact_path = None
                found_error = False
                for log_entry in log.LogSource.from_env(env).read_log():
                    if (event := log_entry.log_event) is not None:
                        if (error_message := event.error) is not None:
                            if (
                                "Your output directory already exists"
                                in error_message
                            ):
                                found_error = True
                                break

                self.assertTrue(
                    found_error,
                    "Expected to find an error about output directory existing",
                )
