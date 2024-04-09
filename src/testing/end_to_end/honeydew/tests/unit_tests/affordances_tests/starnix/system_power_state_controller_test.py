#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.starnix.system_power_state_controller.py."""

import unittest
from unittest import mock

from honeydew import errors
from honeydew.affordances.starnix import (
    system_power_state_controller as starnix_system_power_state_controller,
)
from honeydew.interfaces.affordances import (
    system_power_state_controller as system_power_state_controller_interface,
)
from honeydew.transports import ffx as ffx_transport

_INPUT_ARGS: dict[str, object] = {
    "device_name": "fuchsia-emulator",
}


# pylint: disable=protected-access
class SystemPowerStateControllerStarnixTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.starnix.system_power_state_controller.py."""

    def setUp(self) -> None:
        super().setUp()

        self.mock_ffx = mock.MagicMock(spec=ffx_transport.FFX)

        with mock.patch.object(
            starnix_system_power_state_controller.SystemPowerStateController,
            "_run_starnix_console_shell_cmd",
            autospec=True,
        ) as mock_run_starnix_console_shell_cmd:
            self.system_power_state_controller_obj = starnix_system_power_state_controller.SystemPowerStateController(
                ffx=self.mock_ffx,
                device_name=str(_INPUT_ARGS["device_name"]),
            )

            mock_run_starnix_console_shell_cmd.assert_called_once()

    @mock.patch.object(
        starnix_system_power_state_controller.SystemPowerStateController,
        "suspend_resume",
        autospec=True,
    )
    def test_idle_suspend_auto_resume(self, mock_suspend_resume) -> None:
        """Test case for SystemPowerStateController.idle_suspend_auto_resume()"""

        self.system_power_state_controller_obj.idle_suspend_auto_resume()
        mock_suspend_resume.assert_called_once_with(
            mock.ANY,
            suspend_state=system_power_state_controller_interface.IdleSuspend(),
            resume_mode=system_power_state_controller_interface.AutomaticResume(),
        )

    @mock.patch.object(
        starnix_system_power_state_controller.SystemPowerStateController,
        "_verify_suspend_resume",
        autospec=True,
    )
    @mock.patch.object(
        starnix_system_power_state_controller.SystemPowerStateController,
        "_run_starnix_console_shell_cmd",
        autospec=True,
    )
    def test_suspend_resume_to_do_idle_suspend_auto_resume(
        self, mock_run_starnix_console_shell_cmd, mock_verify_suspend_resume
    ) -> None:
        """Test case for SystemPowerStateController.suspend_resume()"""
        self.system_power_state_controller_obj.suspend_resume(
            suspend_state=system_power_state_controller_interface.IdleSuspend(),
            resume_mode=system_power_state_controller_interface.AutomaticResume(),
        )

        mock_run_starnix_console_shell_cmd.assert_called_once_with(
            mock.ANY,
            cmd=starnix_system_power_state_controller._StarnixCmds.IDLE_SUSPEND,
            timeout=None,
        )
        mock_verify_suspend_resume.assert_called_once_with(
            mock.ANY,
            suspend_state=system_power_state_controller_interface.IdleSuspend(),
            resume_mode=system_power_state_controller_interface.AutomaticResume(),
            duration=mock.ANY,
        )

    @mock.patch.object(
        starnix_system_power_state_controller.SystemPowerStateController,
        "_run_starnix_console_shell_cmd",
        autospec=True,
    )
    def test_verify_suspend_resume_for_idle_suspend_auto_resume(
        self, mock_run_starnix_console_shell_cmd
    ) -> None:
        """Test case for SystemPowerStateController._verify_suspend_resume()
        raising an exception."""
        with self.assertRaisesRegex(
            errors.SystemPowerStateControllerError,
            "Putting the 'fuchsia-emulator' into 'IdleSuspend' followed by "
            "'AutomaticResume after .+sec' operation took .+ seconds instead "
            "of .+ seconds",
        ):
            self.system_power_state_controller_obj.suspend_resume(
                suspend_state=system_power_state_controller_interface.IdleSuspend(),
                resume_mode=system_power_state_controller_interface.AutomaticResume(),
            )

        mock_run_starnix_console_shell_cmd.assert_called_once_with(
            mock.ANY,
            cmd=starnix_system_power_state_controller._StarnixCmds.IDLE_SUSPEND,
            timeout=None,
        )

    def test_suspend_resume_with_not_supported_suspend_mode(self) -> None:
        """Test case for SystemPowerStateController.suspend_resume() raising
        NotSupportedError for suspend operation."""

        with self.assertRaises(errors.NotSupportedError):
            self.system_power_state_controller_obj.suspend_resume(
                suspend_state="invalid",  # type: ignore[arg-type]
                resume_mode=system_power_state_controller_interface.AutomaticResume(),
            )

    def test_suspend_resume_with_not_supported_resume_mode(self) -> None:
        """Test case for SystemPowerStateController.suspend_resume() raising
        NotSupportedError for suspend operation."""
        with self.assertRaises(errors.NotSupportedError):
            self.system_power_state_controller_obj.suspend_resume(
                suspend_state=system_power_state_controller_interface.IdleSuspend(),
                resume_mode=system_power_state_controller_interface.ButtonPressResume(),
            )

    @mock.patch(
        "os.read",
        return_value=starnix_system_power_state_controller._RegExPatterns.STARNIX_CMD_SUCCESS.pattern.encode(),
        autospec=True,
    )
    @mock.patch(
        "pty.openpty",
        return_value=(1, 1),
        autospec=True,
    )
    def test_run_starnix_console_shell_cmd(
        self, mock_openpty, mock_os_read
    ) -> None:
        """Test case for SystemPowerStateController._run_starnix_console_shell_cmd()"""
        self.system_power_state_controller_obj._run_starnix_console_shell_cmd(
            cmd=["something"]
        )
        mock_openpty.assert_called_once()
        mock_os_read.assert_called_once()

    @mock.patch(
        "os.read",
        return_value=starnix_system_power_state_controller._RegExPatterns.STARNIX_NOT_SUPPORTED.pattern.encode(),
        autospec=True,
    )
    @mock.patch(
        "pty.openpty",
        return_value=(1, 1),
        autospec=True,
    )
    def test_run_starnix_console_shell_cmd_raises_not_supported_error(
        self, mock_openpty, mock_os_read
    ) -> None:
        """Test case for SystemPowerStateController._run_starnix_console_shell_cmd()
        raising NotSupportedError"""
        with self.assertRaises(errors.NotSupportedError):
            self.system_power_state_controller_obj._run_starnix_console_shell_cmd(
                cmd=["something"]
            )
        mock_openpty.assert_called_once()
        mock_os_read.assert_called_once()

    @mock.patch(
        "os.read",
        return_value="something".encode(),
        autospec=True,
    )
    @mock.patch(
        "pty.openpty",
        return_value=(1, 1),
        autospec=True,
    )
    def test_run_starnix_console_shell_cmd_raises_starnix_error(
        self, mock_openpty, mock_os_read
    ) -> None:
        """Test case for SystemPowerStateController._run_starnix_console_shell_cmd()
        raising StarnixError"""
        with self.assertRaises(errors.StarnixError):
            self.system_power_state_controller_obj._run_starnix_console_shell_cmd(
                cmd=["something"]
            )
        mock_openpty.assert_called_once()
        mock_os_read.assert_called_once()
