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


class TestMainIntegration(unittest.IsolatedAsyncioTestCase):
    """Integration tests for the main entrypoint.

    These tests encapsulate several real-world invocations of fx test,
    with mocked dependencies.
    """

    DEVICE_TESTS_IN_INPUT = 1
    HOST_TESTS_IN_INPUT = 1
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

        with open(os.path.join(self.fuchsia_dir.name, ".fx-build-dir"), "w") as f:
            f.write("out/default")

        self.out_dir = os.path.join(self.fuchsia_dir.name, "out/default")
        os.makedirs(self.out_dir)

        for name in ["tests.json", "test-list.json"]:
            shutil.copy(
                os.path.join(self.test_data_path, name),
                os.path.join(self.out_dir, name),
            )

        return super().setUp()

    def _mock_run_commands_in_parallel(self, stdout) -> mock.MagicMock:
        m = mock.AsyncMock(return_value=[mock.MagicMock(stdout=stdout)])
        patch = mock.patch("main.run_commands_in_parallel", m)
        patch.start()
        self.addCleanup(patch.stop)
        return m

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

    async def test_suggestions(self):
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
