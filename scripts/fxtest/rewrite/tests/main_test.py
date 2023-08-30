# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os
import shutil
import tempfile
import typing
import unittest
import unittest.mock as mock

from parameterized import parameterized

import args
import event
import main
import util.command


class TestMainIntegration(unittest.IsolatedAsyncioTestCase):
    """Integration tests for the main entrypoint.

    These tests encapsulate several real-world invocations of fx test,
    with mocked dependencies.
    """

    DEVICE_TESTS_IN_INPUT = 1
    HOST_TESTS_IN_INPUT = 2
    TOTAL_TESTS_IN_INPUT = DEVICE_TESTS_IN_INPUT + HOST_TESTS_IN_INPUT

    def setUp(self) -> None:
        # Set up a Fake fuchsia directory.
        self.fuchsia_dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.fuchsia_dir.cleanup)

        # Set up mocks
        self.mocks = []  # type:ignore
        self.mocks.append(
            mock.patch("os.environ", {"FUCHSIA_DIR": self.fuchsia_dir.name})
        )
        for m in self.mocks:
            m.start()
            self.addCleanup(m.stop)

        # Correct for location of the test data files between coverage.py
        # script and how tests are run in-tree.
        cur_path = os.path.dirname(__file__)
        while not os.path.isdir(cur_path):
            cur_path = os.path.split(cur_path)[0]

        self.test_data_path = os.path.join(cur_path, "test_data/build_output")

        self.assertTrue(
            os.path.isfile(os.path.join(self.test_data_path, "tests.json")),
            f"path was {self.test_data_path} for {__file__}",
        )
        self.assertTrue(
            os.path.isfile(os.path.join(self.test_data_path, "test-list.json")),
            f"path was {self.test_data_path} for {__file__}",
        )
        self.assertTrue(
            os.path.isfile(
                os.path.join(self.test_data_path, "package-repositories.json")
            ),
            f"path was {self.test_data_path} for {__file__}",
        )

        with open(os.path.join(self.fuchsia_dir.name, ".fx-build-dir"), "w") as f:
            f.write("out/default")

        self.out_dir = os.path.join(self.fuchsia_dir.name, "out/default")
        os.makedirs(self.out_dir)

        for name in [
            "tests.json",
            "test-list.json",
            "package-repositories.json",
            "package-targets.json",
        ]:
            shutil.copy(
                os.path.join(self.test_data_path, name),
                os.path.join(self.out_dir, name),
            )

        return super().setUp()

    def _mock_run_commands_in_parallel(self, stdout: str) -> mock.MagicMock:
        m = mock.AsyncMock(return_value=[mock.MagicMock(stdout=stdout)])
        patch = mock.patch("main.run_commands_in_parallel", m)
        patch.start()
        self.addCleanup(patch.stop)
        return m

    def _mock_run_command(self, return_code: int) -> mock.MagicMock:
        m = mock.AsyncMock(
            return_value=mock.MagicMock(
                return_code=return_code, stdout="", stderr="", was_timeout=False
            )
        )
        patch = mock.patch("main.execution.run_command", m)
        patch.start()
        self.addCleanup(patch.stop)
        return m

    def _mock_has_device_connected(self, value: bool):
        m = mock.AsyncMock(return_value=value)
        patch = mock.patch("main.has_device_connected", m)
        patch.start()
        self.addCleanup(patch.stop)

    def _mock_has_tests_in_base(self, value: bool):
        m = mock.MagicMock(return_value=value)
        patch = mock.patch("main.has_tests_in_base", m)
        patch.start()
        self.addCleanup(patch.stop)

    def _make_call_args_prefix_set(
        self, call_list: mock._CallList
    ) -> typing.Set[typing.Tuple]:
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
            typing.Set[typing.List[typing.Any]]: Set of prefixes to calls.
        """
        ret: typing.Set[typing.Tuple] = set()
        for call in call_list:
            args, _ = call
            cur = []
            for a in args:
                cur.append(a)
                ret.add(tuple(cur))

        return ret

    def assertIsSubset(
        self, subset: typing.Set[typing.Any], full: typing.Set[typing.Any]
    ):
        inter = full.intersection(subset)
        self.assertEqual(
            inter, subset, f"Full set was\n {self.prettyFormatPrefixes(full)}"
        )

    def prettyFormatPrefixes(self, vals: typing.Set[typing.Any]) -> str:
        return "\n ".join(map(lambda x: " ".join(x), sorted(vals)))

    async def test_dry_run(self):
        """Test a basic dry run of the command."""
        recorder = event.EventRecorder()
        ret = await main.async_main_wrapper(
            args.parse_args(["--simple", "--dry"]), recorder=recorder
        )
        self.assertEqual(ret, 0)

        selection_events: typing.List[event.TestSelectionPayload] = [
            e.payload.test_selections
            async for e in recorder.iter()
            if e.payload is not None and e.payload.test_selections is not None
        ]

        self.assertEqual(len(selection_events), 1)
        selection_event = selection_events[0]
        self.assertEqual(len(selection_event.selected), self.TOTAL_TESTS_IN_INPUT)

    async def test_fuzzy_dry_run(self):
        """Test a dry run of the command for fuzzy matching"""
        recorder = event.EventRecorder()
        ret = await main.async_main_wrapper(
            args.parse_args(["--simple", "--dry", "--fuzzy=1", "foo_test"]),
            recorder=recorder,
        )
        self.assertEqual(ret, 0)

        selection_events: typing.List[event.TestSelectionPayload] = [
            e.payload.test_selections
            async for e in recorder.iter()
            if e.payload is not None and e.payload.test_selections is not None
        ]

        self.assertEqual(len(selection_events), 1)
        selection_event = selection_events[0]
        self.assertEqual(len(selection_event.selected), 1)

    @parameterized.expand(
        [("--host", HOST_TESTS_IN_INPUT), ("--device", DEVICE_TESTS_IN_INPUT)]
    )
    async def test_selection_flags(self, flag_name: str, expected_count: int):
        """Test that the correct --device or --host tests are selected"""

        recorder = event.EventRecorder()
        ret = await main.async_main_wrapper(
            args.parse_args(["--simple", "--dry"] + [flag_name]), recorder=recorder
        )
        self.assertEqual(ret, 0)

        selection_events: typing.List[event.TestSelectionPayload] = [
            e.payload.test_selections
            async for e in recorder.iter()
            if e.payload is not None and e.payload.test_selections is not None
        ]

        self.assertEqual(len(selection_events), 1)
        selection_event = selection_events[0]
        self.assertEqual(len(selection_event.selected), expected_count)

    @parameterized.expand(
        [("--use-package-hash", DEVICE_TESTS_IN_INPUT), ("--no-use-package-hash", 0)]
    )
    async def test_use_package_hash(self, flag_name: str, expected_hash_matches: int):
        """Test ?hash= is used only when --use-package-hash is set"""

        command_mock = self._mock_run_command(0)
        self._mock_has_device_connected(True)
        self._mock_has_tests_in_base(False)

        ret = await main.async_main_wrapper(
            args.parse_args(["--simple", "--no-build"] + [flag_name])
        )
        self.assertEqual(ret, 0)

        call_prefixes = self._make_call_args_prefix_set(command_mock.call_args_list)

        self.assertIsSubset(
            {
                ("fx", "ffx", "test", "run"),
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

    async def test_suggestions(self):
        """Test that targets are suggested when there are no test matches."""
        mocked_commands = self._mock_run_commands_in_parallel("No matches")
        ret = await main.async_main_wrapper(
            args.parse_args(["--simple", "non_existant_test_does_not_match"])
        )
        self.assertEqual(ret, 1)
        self.assertListEqual(
            mocked_commands.call_args[0][0],
            [
                [
                    "fx",
                    "search-tests",
                    "--max-results=6",
                    "non_existant_test_does_not_match",
                    "--no-color",
                ]
            ],
        )

        # TODO(b/295340412): Test that suggestions are suppressed.

    async def test_full_success(self):
        """Test that we can run all tests and report success"""

        command_mock = self._mock_run_command(0)
        self._mock_has_device_connected(True)
        self._mock_has_tests_in_base(False)

        ret = await main.async_main_wrapper(args.parse_args(["--simple"]))
        self.assertEqual(ret, 0)

        call_prefixes = self._make_call_args_prefix_set(command_mock.call_args_list)

        # Make sure we built, published, and ran the device test.
        self.assertIsSubset(
            {
                ("fx", "build", "src/sys:foo_test_package", "host_x64/bar_test"),
                ("fx", "ffx", "repository", "publish"),
                ("fx", "ffx", "test", "run"),
            },
            call_prefixes,
        )

        # Make sure we ran the host tests.
        self.assertTrue(any(["bar_test" in v[0] for v in call_prefixes]))
        self.assertTrue(any(["baz_test" in v[0] for v in call_prefixes]))

    async def test_no_build(self):
        """Test that we can run all tests and report success"""

        command_mock = self._mock_run_command(0)
        self._mock_has_device_connected(True)
        self._mock_has_tests_in_base(False)

        ret = await main.async_main_wrapper(args.parse_args(["--simple", "--no-build"]))
        self.assertEqual(ret, 0)

        call_prefixes = self._make_call_args_prefix_set(command_mock.call_args_list)

        self.assertFalse(("fx", "build") in call_prefixes)
        self.assertFalse(("fx", "ffx", "repository", "publish") in call_prefixes)

        self.assertIsSubset(
            {
                ("fx", "ffx", "test", "run"),
            },
            call_prefixes,
        )

        # Make sure we ran the host test.
        self.assertTrue(any(["bar_test" in v[0] for v in call_prefixes]))
        self.assertTrue(any(["baz_test" in v[0] for v in call_prefixes]))

    async def test_first_failure(self):
        """Test that one failing test aborts the rest with --fail"""

        command_mock = self._mock_run_command(1)
        command_mock.side_effect = [
            util.command.CommandOutput("out", "err", 1, 10, None),
            util.command.CommandOutput("out", "err", 1, 10, None),
            util.command.CommandOutput("out", "err", 1, 10, None),
        ]

        self._mock_has_device_connected(True)
        self._mock_has_tests_in_base(False)

        ret = await main.async_main_wrapper(
            args.parse_args(["--simple", "--no-build", "--fail"])
        )

        # bar_test and baz_test are not hermetic, so cannot run at the same time.
        # One of them will run before the other, which means --fail
        # prevents one of them from starting, and we expect to see
        # only bar_test (since baz_test is defined later in the file)
        call_prefixes = self._make_call_args_prefix_set(command_mock.call_args_list)
        self.assertEqual(ret, 1)

        self.assertTrue(any(["bar_test" in v[0] for v in call_prefixes]))
        self.assertFalse(any(["baz_test" in v[0] for v in call_prefixes]))

    async def test_count(self):
        """Test that we can re-run a test multiple times with --count"""

        command_mock = self._mock_run_command(0)

        self._mock_has_device_connected(True)
        self._mock_has_tests_in_base(False)

        # Run each test 3 times, no parallel to better match behavior of failure case test.
        ret = await main.async_main_wrapper(
            args.parse_args(["--simple", "--no-build", "--count=3", "--parallel=1"])
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

    async def test_count_with_timeout(self):
        """Test that we abort running the rest of the tests in a --count group if a timeout occurs."""

        command_mock = self._mock_run_command(1)
        command_mock.return_value = util.command.CommandOutput(
            "", "", 1, 10, None, was_timeout=True
        )

        self._mock_has_device_connected(True)
        self._mock_has_tests_in_base(False)

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
