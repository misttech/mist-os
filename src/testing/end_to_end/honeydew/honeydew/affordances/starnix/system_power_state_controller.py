#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""SystemPowerStateController affordance implementation using sysfs."""

import enum
import logging
import os
import pty
import re
import subprocess
import time
from typing import Any

from honeydew import errors
from honeydew.interfaces.affordances import (
    system_power_state_controller as system_power_state_controller_interface,
)
from honeydew.transports import ffx as ffx_transport


class _StarnixCmds:
    """Class to hold Starnix commands."""

    PREFIX: list[str] = [
        "starnix",
        "console",
        "/bin/sh",
        "-c",
    ]

    IDLE_SUSPEND: list[str] = [
        "echo -n mem > /sys/power/state",
    ]

    IS_STARNIX_SUPPORTED: list[str] = [
        "echo hello",
    ]


class _Timeouts(enum.IntEnum):
    """Class to hold the timeouts."""

    STARNIX_CMD = 15


class _RegExPatterns:
    STARNIX_CMD_SUCCESS: re.Pattern[str] = re.compile(r"(exit code: 0)")
    STARNIX_NOT_SUPPORTED: re.Pattern[str] = re.compile(
        r"Unable to find Starnix container in the session"
    )


_MAX_READ_SIZE: int = 1024


_LOGGER: logging.Logger = logging.getLogger(__name__)


class SystemPowerStateController(
    system_power_state_controller_interface.SystemPowerStateController
):
    """SystemPowerStateController affordance implementation using sysfs.

    Args:
        device_name: Device name returned by `ffx target list`.
        ffx: FFX transport.

    Raises:
        errors.NotSupportedError: If Fuchsia device does not support Starnix.
    """

    def __init__(self, device_name: str, ffx: ffx_transport.FFX) -> None:
        self._device_name: str = device_name
        self._ffx: ffx_transport.FFX = ffx

        _LOGGER.debug(
            "Checking if %s supports %s affordance...",
            self._device_name,
            self.__class__.__name__,
        )
        self._run_starnix_console_shell_cmd(
            cmd=_StarnixCmds.IS_STARNIX_SUPPORTED
        )
        _LOGGER.debug(
            "%s does support %s affordance...",
            self._device_name,
            self.__class__.__name__,
        )

    # List all the public methods
    def suspend_resume(
        self,
        suspend_state: system_power_state_controller_interface.SuspendState,
        resume_mode: system_power_state_controller_interface.ResumeMode,
    ) -> None:
        """Perform suspend-resume operation on the device.

        This is a synchronous operation on the device and thus this call will be
        hanged until resume operation finishes.

        Args:
            suspend_state: Which state to suspend the Fuchsia device into.
            resume_mode: Information about how to resume the device.

        Raises:
            errors.SystemPowerStateControllerError: In case of failure
            errors.NotSupportedError: If any of the suspend_state or resume_type
                is not yet supported
        """
        _LOGGER.info(
            "Putting the '%s' into '%s' followed by '%s'...",
            self._device_name,
            suspend_state,
            resume_mode,
        )

        start_time: float = time.time()

        if isinstance(
            resume_mode, system_power_state_controller_interface.AutomaticResume
        ):
            pass
            # Device will resume automatically
        else:
            raise errors.NotSupportedError(
                f"Resuming the device using '{resume_mode}' is not yet supported."
            )

        if isinstance(
            suspend_state, system_power_state_controller_interface.IdleSuspend
        ):
            self._perform_idle_suspend()
        else:
            raise errors.NotSupportedError(
                f"Suspending the device to '{suspend_state}' state is not yet "
                f"supported."
            )

        end_time: float = time.time()
        duration: float = end_time - start_time

        self._verify_suspend_resume(suspend_state, resume_mode, duration)

        _LOGGER.info(
            "Successfully completed '%s' and '%s' operations on '%s' in '%s' seconds",
            suspend_state,
            resume_mode,
            self._device_name,
            duration,
        )

    def idle_suspend_auto_resume(self) -> None:
        """Perform idle-suspend and auto-resume operation on the device.

        Raises:
            errors.SystemPowerStateControllerError: In case of failure
        """
        self.suspend_resume(
            suspend_state=system_power_state_controller_interface.IdleSuspend(),
            resume_mode=system_power_state_controller_interface.AutomaticResume(),
        )

    # List all the private methods
    def _perform_idle_suspend(self) -> None:
        """Perform Idle mode suspend operation.

        Raises:
            errors.SystemPowerStateControllerError: In case of failure.
        """
        try:
            self._run_starnix_console_shell_cmd(
                cmd=_StarnixCmds.IDLE_SUSPEND, timeout=None
            )
        except Exception as err:  # pylint: disable=broad-except
            raise errors.SystemPowerStateControllerError(
                f"Failed to put {self._device_name} into idle-suspend mode"
            ) from err

    def _run_starnix_console_shell_cmd(
        self, cmd: list[str], timeout: float | None = _Timeouts.STARNIX_CMD
    ) -> str:
        """Run a starnix console command and return its output.

        Args:
            cmd: cmd that need to be run excluding `starnix /bin/sh -c`.
            timeout: command timeout.

        Returns:
            Output of `ffx -t {target} starnix /bin/sh -c {cmd}`.

        Raises:
            errors.StarnixError: In case of starnix command failure.
            errors.NotSupportedError: If Fuchsia device does not support Starnix.
            subprocess.TimeoutExpired: In case of command timeout.
        """
        # starnix console requires the process to run in tty:
        host_fd: int
        child_fd: int
        host_fd, child_fd = pty.openpty()

        starnix_cmd: list[str] = _StarnixCmds.PREFIX + cmd
        starnix_cmd_str: str = " ".join(starnix_cmd)
        process: subprocess.Popen[Any] = self._ffx.popen(
            cmd=starnix_cmd,
            stdin=child_fd,
            stdout=child_fd,
            stderr=child_fd,
        )
        process.wait(timeout)

        # Note: This call may sometime return less chars than _MAX_READ_SIZE
        # even when command output contains more chars. This happened with
        # `getprop` command output but not with suspend-resume related
        # operations. So consider exploring better ways to read command output
        # such that this method can be used with other starnix console commands
        output: str = os.read(host_fd, _MAX_READ_SIZE).decode("utf-8")

        _LOGGER.debug(
            "Starnix console cmd `%s` completed. returncode=%s, output:\n%s",
            starnix_cmd_str,
            process.returncode,
            output,
        )

        if _RegExPatterns.STARNIX_CMD_SUCCESS.search(output):
            return output
        elif _RegExPatterns.STARNIX_NOT_SUPPORTED.search(output):
            board: str | None = None
            product: str | None = None
            try:
                board = self._ffx.get_target_board()
                product = self._ffx.get_target_product()
            except Exception:  # pylint: disable=broad-except
                pass
            error_msg: str
            if board and product:
                error_msg = (
                    f"{self._device_name} running {product}.{board} does not "
                    f"support Starnix"
                )
            else:
                error_msg = f"{self._device_name} does not support Starnix"
            raise errors.NotSupportedError(error_msg)
        else:
            raise errors.StarnixError(
                f"Starnix console cmd `{starnix_cmd_str}` failed. (See debug "
                "logs for command output)"
            )

    def _verify_suspend_resume(
        self,
        suspend_state: system_power_state_controller_interface.SuspendState,
        resume_mode: system_power_state_controller_interface.ResumeMode,
        duration: float,
    ) -> None:
        """Verifies suspend resume operation has been indeed performed
        correctly.

        Args:
            suspend_state: Which state to suspend the Fuchsia device into.
            resume_mode: Information about how to resume the device.
            duration: how long suspend-resume operation took.

        Raises:
            errors.SystemPowerStateControllerError: In case of verification
                failure.
        """
        if isinstance(
            resume_mode, system_power_state_controller_interface.AutomaticResume
        ):
            buffer_duration: float = 5
            max_expected_duration: float = (
                resume_mode.duration + buffer_duration
            )
            actual_duration: float = duration

            if (
                actual_duration < resume_mode.duration
                or actual_duration > max_expected_duration
            ):
                raise errors.SystemPowerStateControllerError(
                    f"Putting the '{self._device_name}' into '{suspend_state}' "
                    f"followed by '{resume_mode}' operation took {duration} "
                    f"seconds instead of {resume_mode.duration} seconds. "
                    f"Expected duration range: [{resume_mode.duration}, "
                    f"{max_expected_duration}] seconds.",
                )
