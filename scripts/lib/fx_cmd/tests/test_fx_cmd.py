# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import asyncio
import pathlib
import tempfile
import unittest
import unittest.mock as mock

import fx_cmd
from async_utils.command import StderrEvent, StdoutEvent

real_subprocess_exec = asyncio.create_subprocess_exec


class TestFxCmd(unittest.TestCase):
    @mock.patch.dict("os.environ", {"FUCHSIA_BUILD_DIR": "/tmp/fuchsia/global"})
    def test_command_line(self) -> None:
        """command line respects build directory specification"""
        cmd = fx_cmd.FxCmd("format-code")
        self.assertEqual(
            cmd.command_line,
            ["fx", "--dir", "/tmp/fuchsia/global", "format-code"],
        )
        cmd = fx_cmd.FxCmd(
            "format-code", build_directory=pathlib.Path("/fuchsia")
        )
        self.assertEqual(
            cmd.command_line, ["fx", "--dir", "/fuchsia", "format-code"]
        )

    # This mock captures the --dir argument to an `fx` command and lists
    # that directory rather than running the original command.
    @mock.patch(
        "asyncio.subprocess.create_subprocess_exec",
        mock.Mock(
            side_effect=lambda *args, **kwargs: real_subprocess_exec(
                "ls", args[2], **kwargs
            ),
        ),
    )
    def test_sync_stdout(self) -> None:
        """sync execution returns stdout"""
        with tempfile.TemporaryDirectory() as td:
            path = pathlib.Path(td)
            f1 = path / "foo.txt"
            f2 = path / "bar.txt"
            f1.touch()
            f2.touch()

            lines: list[str] = []

            def callback(x: StdoutEvent) -> None:
                lines.append(x.text.decode(errors="ignore").strip())

            result = fx_cmd.FxCmd("build", build_directory=path).sync(
                stdout_callback=callback
            )
            self.assertEqual(result.return_code, 0)

            self.assertTrue(
                any(["bar.txt" in l for l in lines]),
                f"Missing bar.txt in {lines}",
            )
            self.assertTrue(
                any(["foo.txt" in l for l in lines]),
                f"Missing foo.txt in {lines}",
            )

    @mock.patch(
        "asyncio.subprocess.create_subprocess_exec",
        mock.Mock(
            side_effect=lambda *args, **kwargs: real_subprocess_exec(
                "ls", "/tmp/does-not-exist12345", **kwargs
            ),
        ),
    )
    def test_sync_stderr(self) -> None:
        """sync execution returns stderr"""
        lines: list[str] = []

        def callback(x: StderrEvent) -> None:
            lines.append(x.text.decode(errors="ignore").strip())

        result = fx_cmd.FxCmd(
            "build", build_directory=pathlib.Path("/missing")
        ).sync(stderr_callback=callback)
        self.assertNotEqual(result.return_code, 0)

        self.assertNotEqual(lines, [])
