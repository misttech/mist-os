#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.sl4f.screenshot.py."""

import unittest
from pathlib import Path
from unittest import mock

from honeydew.affordances.ffx.ui import screenshot as ffx_screenshot
from honeydew.transports import ffx as ffx_transport


class ScreenshotFfxTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.ffx.ui.screenshot.py."""

    def setUp(self) -> None:
        super().setUp()
        self.mock_ffx = mock.MagicMock(spec=ffx_transport.FFX)
        self.screenshot_obj = ffx_screenshot.Screenshot(ffx=self.mock_ffx)

    def test_take_screenshot(self) -> None:
        # An image with a single bgra green pixel:
        expected_img_bytes = b"\x00\xff\x00\xff"

        def mock_run(cmd: list[str]) -> str:
            expected_cmd = ["target", "screenshot", "--format", "bgra", "-d"]
            self.assertEqual(expected_cmd, cmd[0:-1])
            output_dir = Path(cmd[-1])
            output_file = output_dir / "screenshot.bgra"
            output_file.write_bytes(expected_img_bytes)
            return f"output: {output_file}"

        self.mock_ffx.run = mock.MagicMock(side_effect=mock_run)

        img = self.screenshot_obj.take()

        self.mock_ffx.run.assert_called_once()
        self.assertEqual(img.size.width, 1.0)
        self.assertEqual(img.size.height, 1.0)
        self.assertEqual(img.data, expected_img_bytes)
