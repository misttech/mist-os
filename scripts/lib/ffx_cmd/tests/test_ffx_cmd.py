# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os
import pathlib
import unittest

import ffx_cmd
import fx_cmd
from async_utils.command import AsyncCommand


class DirectExecutor(fx_cmd.ExecutableCommand):
    async def start(self, *args: str) -> AsyncCommand:
        index_of_ffx = 0
        for i, val in enumerate(args):
            if val == "ffx":
                index_of_ffx = i
                break
        return await AsyncCommand.create(
            "host-tools/ffx", *args[index_of_ffx + 1 :]
        )


class TestFfxCmd(unittest.TestCase):
    def test_command_line(self) -> None:
        """command lines respect output format flag"""
        inner = fx_cmd.FxCmd(build_directory=pathlib.Path("/fuchsia"))
        actual = ffx_cmd.FfxCmd(inner=inner).command_line("foo")
        self.assertEqual(actual, ["ffx", "foo"])

        actual = ffx_cmd.FfxCmd(
            inner=inner, output_format=ffx_cmd.FfxOutputFormat.JSON
        ).command_line("foo")
        self.assertEqual(actual, ["ffx", "--machine", "json", "foo"])

        actual = ffx_cmd.FfxCmd(
            inner=inner, output_format=ffx_cmd.FfxOutputFormat.PRETTY_JSON
        ).command_line("foo")
        self.assertEqual(actual, ["ffx", "--machine", "json-pretty", "foo"])

    def test_try_run(self) -> None:
        version = ffx_cmd.version(inner=DirectExecutor()).sync()
        self.assertGreater(version.api_level, 0)
