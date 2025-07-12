# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.ui.scenic.py."""

import unittest
from unittest import mock

from parameterized import parameterized

from honeydew.affordances.ui.scenic import errors, scenic_using_ffx
from honeydew.transports.ffx import errors as ffx_errors
from honeydew.transports.ffx import ffx as ffx_transport


class ScenicUsingFfxTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.ffx.ui.scenic.py."""

    def setUp(self) -> None:
        super().setUp()
        self.mock_ffx = mock.MagicMock(spec=ffx_transport.FFX)
        self.scenic_obj = scenic_using_ffx.ScenicUsingFfx(ffx=self.mock_ffx)

        self.mock_ffx.run.assert_called_once_with(
            ["component", "show", "core/ui/scenic"]
        )
        self.mock_ffx.run.reset_mock()

    @parameterized.expand(
        [
            ("vulkan", 'renderer -> String("vulkan")', "vulkan"),
            ("cpu", 'renderer -> String("cpu")', "cpu"),
            ("unknown", 'renderer -> String("a")', "a"),
        ]
    )
    def test_renderer(self, unused_name: str, ffx_ret: str, want: str) -> None:
        self.mock_ffx.run.return_value = ffx_ret
        self.assertEqual(self.scenic_obj.renderer(), want)
        self.mock_ffx.run.assert_called_once_with(
            ["component", "show", "core/ui/scenic"]
        )

    def test_renderer_not_found(self) -> None:
        with self.assertRaises(errors.ScenicError):
            self.scenic_obj.renderer()

        self.mock_ffx.run.assert_called_once_with(
            ["component", "show", "core/ui/scenic"]
        )

    def test_renderer_error(self) -> None:
        self.mock_ffx.run.side_effect = ffx_errors.FfxCommandError("ffx error")

        with self.assertRaises(errors.ScenicError):
            self.scenic_obj.renderer()

        self.mock_ffx.run.assert_called_once_with(
            ["component", "show", "core/ui/scenic"]
        )
