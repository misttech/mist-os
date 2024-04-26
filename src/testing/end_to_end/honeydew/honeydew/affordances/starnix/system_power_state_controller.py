#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""SystemPowerStateController affordance implementation using startnix."""

import contextlib
import enum
import io
import logging
import os
import pty
import re
import subprocess
import time
import typing
from collections.abc import Generator

from honeydew import errors
from honeydew.interfaces.affordances import (
    system_power_state_controller as system_power_state_controller_interface,
)
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import ffx as ffx_transport
from honeydew.typing import custom_types


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


class _FuchsiaCmds:
    """Class to hold Fuchsia commands."""

    SSH_PREFIX: list[str] = [
        "target",
        "ssh",
    ]

    @staticmethod
    def set_timer_command(duration: int) -> str:
        return f"hrtimer-ctl --id 2 --event {duration}"


class _Timeouts(enum.IntEnum):
    """Class to hold the timeouts."""

    STARNIX_CMD = 15
    FFX_LOGS_CMD = 60


class _RegExPatterns:
    """Class to hold Regular Expression patterns."""

    STARNIX_CMD_SUCCESS: re.Pattern[str] = re.compile(r"(exit code: 0)")

    STARNIX_NOT_SUPPORTED: re.Pattern[str] = re.compile(
        r"Unable to find Starnix container in the session"
    )

    SUSPEND_OPERATION: re.Pattern[str] = re.compile(
        r"\[(\d+?\.\d+?)\].*?\[system-activity-governor\].+?Suspending"
    )

    RESUME_OPERATION: re.Pattern[str] = re.compile(
        r"\[(\d+?\.\d+?)\].*?\[system-activity-governor\].+?Resuming.+?Ok"
    )

    HONEYDEW_SUSPEND_RESUME_START: re.Pattern[str] = re.compile(
        r"\[lacewing\].*?\[Host Time: (.*?)\].*?Performing.*?Suspend.*?followed by.*?Resume.*?operations"
    )

    HONEYDEW_SUSPEND_RESUME_END: re.Pattern[str] = re.compile(
        r"\[lacewing\].*?\[Host Time: (.*?)\].*?Completed.*?Suspend.*?followed by.*?Resume.*?operations.*?in (\d+?.\d+?) seconds"
    )

    SUSPEND_RESUME_PATTERNS: list[re.Pattern[str]] = [
        HONEYDEW_SUSPEND_RESUME_START,
        SUSPEND_OPERATION,
        RESUME_OPERATION,
        HONEYDEW_SUSPEND_RESUME_END,
    ]

    TIMER_STARTED: re.Pattern[str] = re.compile(r"Timer started")

    TIMER_ENDED: re.Pattern[str] = re.compile(r"Event trigged")


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

    def __init__(
        self,
        device_name: str,
        ffx: ffx_transport.FFX,
        device_logger: affordances_capable.FuchsiaDeviceLogger,
    ) -> None:
        self._device_name: str = device_name
        self._ffx: ffx_transport.FFX = ffx
        self._device_logger: affordances_capable.FuchsiaDeviceLogger = (
            device_logger
        )

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
        verify: bool = True,
    ) -> None:
        """Perform suspend-resume operation on the device.

        This is a synchronous operation on the device and thus this call will be
        hanged until resume operation finishes.

        Args:
            suspend_state: Which state to suspend the Fuchsia device into.
            resume_mode: Information about how to resume the device.
            verify: Whether or not to verify if suspend-resume operation
                performed successfully. Optional and default is True. Note that
                this raises SystemPowerStateControllerError if verification
                fails.

        Raises:
            errors.SystemPowerStateControllerError: In case of failure
            errors.NotSupportedError: If any of the suspend_state or resume_type
                is not yet supported
        """
        logs_start_time: float = time.time()
        log_message: str = (
            f"Performing '{suspend_state}' followed by '{resume_mode}' "
            f"operations on '{self._device_name}'..."
        )
        _LOGGER.info(log_message)
        self._device_logger.log_message_to_device(
            message=log_message,
            level=custom_types.LEVEL.INFO,
        )

        suspend_resume_start_time: float = time.time()

        with self._set_resume_mode(resume_mode=resume_mode):
            self._suspend(suspend_state=suspend_state)

        suspend_resume_end_time: float = time.time()
        suspend_resume_execution_time: float = (
            suspend_resume_end_time - suspend_resume_start_time
        )

        log_message = (
            f"Completed '{suspend_state}' followed by '{resume_mode}' "
            f"operations on '{self._device_name}' in {suspend_resume_execution_time} "
            f"seconds."
        )
        self._device_logger.log_message_to_device(
            message=log_message,
            level=custom_types.LEVEL.INFO,
        )
        _LOGGER.info(log_message)
        logs_end_time: float = time.time()
        logs_duration: float = logs_end_time - logs_start_time

        if verify:
            self._verify_suspend_resume(
                suspend_state=suspend_state,
                resume_mode=resume_mode,
                suspend_resume_execution_time=suspend_resume_execution_time,
                logs_duration=logs_duration,
            )

    def idle_suspend_auto_resume(self, verify: bool = True) -> None:
        """Perform idle-suspend and auto-resume operation on the device.

        Args:
            verify: Whether or not to verify if suspend-resume operation
                performed successfully. Optional and default is True.

        Raises:
            errors.SystemPowerStateControllerError: In case of failure
        """
        self.suspend_resume(
            suspend_state=system_power_state_controller_interface.IdleSuspend(),
            resume_mode=system_power_state_controller_interface.AutomaticResume(),
            verify=verify,
        )

    def idle_suspend_timer_based_resume(
        self, duration: int, verify: bool = True
    ) -> None:
        """Perform idle-suspend and timer-based-resume operation on the device.

        Args:
            duration: Resume timer duration in seconds.
            verify: Whether or not to verify if suspend-resume operation
                performed successfully. Optional and default is True.

        Raises:
            errors.SystemPowerStateControllerError: In case of failure
        """
        self.suspend_resume(
            suspend_state=system_power_state_controller_interface.IdleSuspend(),
            resume_mode=system_power_state_controller_interface.TimerResume(
                duration=duration,
            ),
            verify=verify,
        )

    # List all the private methods
    def _suspend(
        self,
        suspend_state: system_power_state_controller_interface.SuspendState,
    ) -> None:
        """Perform suspend operation on the device.

        This is a synchronous operation on the device and thus this call will be
        hanged until resume operation finishes.

        Args:
            suspend_state: Which state to suspend the Fuchsia device into.

        Raises:
            errors.SystemPowerStateControllerError: In case of failure
            errors.NotSupportedError: If any of the suspend_state or resume_type
                is not yet supported
        """
        _LOGGER.info(
            "Putting '%s' into '%s'",
            self._device_name,
            suspend_state,
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

        _LOGGER.info(
            "'%s' has been resumed from '%s'",
            self._device_name,
            suspend_state,
        )

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

    @contextlib.contextmanager
    def _set_resume_mode(
        self,
        resume_mode: system_power_state_controller_interface.ResumeMode,
    ) -> Generator[None, None, None]:
        """Perform resume operation on the device.

        This is a synchronous operation on the device and thus call will be
        hanged until resume operation finishes. So we will be using a context
        manager which will start resume mode using subprocess, saves the proc
        and yields and when called again will wait for resume operation to be
        finished using the saved proc.

        Args:
            resume_mode: Information about how to resume the device.

        Raises:
            errors.SystemPowerStateControllerError: In case of failure
            errors.NotSupportedError: If any of the suspend_state or resume_type
                is not yet supported
            errors.HoneydewTimeoutError: If timer has not been started in 2 sec
        """
        _LOGGER.info(
            "Informing '%s' to resume using '%s'",
            self._device_name,
            resume_mode,
        )

        if isinstance(
            resume_mode, system_power_state_controller_interface.AutomaticResume
        ):
            # Nothing to do for AutomaticResume
            pass
        elif isinstance(
            resume_mode, system_power_state_controller_interface.TimerResume
        ):
            proc: subprocess.Popen[str] = self._set_timer(resume_mode.duration)
            self._wait_for_timer_start(proc=proc)
        else:
            raise errors.NotSupportedError(
                f"Resuming the device using '{resume_mode}' is not yet supported."
            )

        yield

        if isinstance(
            resume_mode, system_power_state_controller_interface.AutomaticResume
        ):
            # Device will resume automatically
            return
        elif isinstance(
            resume_mode, system_power_state_controller_interface.TimerResume
        ):
            self._wait_for_timer_end(proc=proc, resume_mode=resume_mode)
        else:
            raise errors.NotSupportedError(
                f"Resuming the device using '{resume_mode}' is not yet supported."
            )

    def _set_timer(self, duration: int) -> subprocess.Popen[str]:
        """Sets the timer.

        Args:
            duration: Resume timer duration in seconds.

        Raises:
            errors.SystemPowerStateControllerError: In case of failure.
        """
        try:
            _LOGGER.info(
                "Setting a timer for '%s sec' on '%s'",
                duration,
                self._device_name,
            )
            proc: subprocess.Popen[str] = self._ffx.popen(
                cmd=_FuchsiaCmds.SSH_PREFIX
                + [_FuchsiaCmds.set_timer_command(duration=duration)],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            return proc
        except Exception as err:  # pylint: disable=broad-except
            raise errors.SystemPowerStateControllerError(
                f"Failed to set timer on {self._device_name}"
            ) from err

    def _wait_for_timer_start(self, proc: subprocess.Popen[str]) -> None:
        """Wait for the timer to start on the device.

        Args:
            proc: process used to set the timer.

        Raises:
            errors.SystemPowerStateControllerError: Timer start failed.
            errors.HoneydewTimeoutError: Wait for timer start resulted in
                timeout.
        """
        timeout: int = 2
        start_time: float = time.time()
        end_time: float = start_time + timeout

        std_out: typing.IO[str] | None = proc.stdout
        if not isinstance(std_out, io.TextIOWrapper):
            raise errors.SystemPowerStateControllerError(
                f"Failed to read hrtimer-ctl output on {self._device_name}"
            )
        while time.time() < end_time:
            try:
                line: str = std_out.readline()
                _LOGGER.debug(
                    "Line read from the hrtimer-ctl command output: %s",
                    line.strip(),
                )

                if _RegExPatterns.TIMER_STARTED.search(line):
                    _LOGGER.info(
                        "Timer has been started on %s", self._device_name
                    )
                    break
                elif line == "":  # End of output
                    raise errors.SystemPowerStateControllerError(
                        "hrtimer-ctl completed without starting a timer"
                    )
            except Exception as err:  # pylint: disable=broad-except
                raise errors.SystemPowerStateControllerError(
                    f"Timer has not been started on {self._device_name}"
                ) from err
        else:
            raise errors.HoneydewTimeoutError(
                f"Timer has not been started on {self._device_name} in "
                f"{timeout} sec"
            )

    def _wait_for_timer_end(
        self,
        proc: subprocess.Popen[str],
        resume_mode: system_power_state_controller_interface.TimerResume,
    ) -> None:
        """Wait for the timer to end on the device.

        Args:
            proc: process used to set the timer.
            resume_mode: Information about how to resume the device.

        Raises:
            errors.SystemPowerStateControllerError: Timer end failed.
        """
        output: str
        error: str
        output, error = proc.communicate(timeout=resume_mode.duration)

        if proc.returncode != 0:
            message: str = (
                f"hrtimer-ctl returned a failure while waiting for the timer "
                f"to end. returncode={proc.returncode}"
            )
            if error:
                message = f"{message}, error='{error}'"
            raise errors.SystemPowerStateControllerError(message)

        if _RegExPatterns.TIMER_ENDED.search(output):
            _LOGGER.info("Timer has been ended on %s", self._device_name)
        else:
            raise errors.SystemPowerStateControllerError(
                "hrtimer-ctl completed without ending the timer"
            )

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
        process: subprocess.Popen[str] = self._ffx.popen(
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
        suspend_resume_execution_time: float,
        logs_duration: float,
    ) -> None:
        """Verifies suspend resume operation has been indeed performed
        correctly.

        Args:
            suspend_state: Which state to suspend the Fuchsia device into.
            resume_mode: Information about how to resume the device.
            suspend_resume_execution_time: How long suspend-resume operation
                took.
            logs_duration: How many seconds of logs need to be captured for
                the log analysis.
        Raises:
            errors.SystemPowerStateControllerError: In case of verification
                failure.
        """
        _LOGGER.info(
            "Verifying the '%s' followed by '%s' operations that were "
            "performed on '%s'...",
            suspend_state,
            resume_mode,
            self._device_name,
        )

        self._verify_suspend_resume_using_duration(
            suspend_state=suspend_state,
            resume_mode=resume_mode,
            suspend_resume_duration=suspend_resume_execution_time,
            min_buffer_duration=0,
            max_buffer_duration=3,
        )

        # TODO (https://fxbug.dev/335494603): Use inspect based verification
        self._verify_suspend_resume_using_log_analysis(
            suspend_state=suspend_state,
            resume_mode=resume_mode,
            logs_duration=logs_duration,
        )

        _LOGGER.info(
            "Successfully verified the '%s' followed by '%s' operations that "
            "were performed on '%s'",
            suspend_state,
            resume_mode,
            self._device_name,
        )

    def _verify_suspend_resume_using_duration(
        self,
        suspend_state: system_power_state_controller_interface.SuspendState,
        resume_mode: system_power_state_controller_interface.ResumeMode,
        suspend_resume_duration: float,
        min_buffer_duration: float,
        max_buffer_duration: float,
    ) -> None:
        """Verify that suspend-resume operation was indeed triggered by checking
        the duration it took to perform suspend-resume operation.

        Args:
            suspend_state: Which state to suspend the Fuchsia device into.
            resume_mode: Information about how to resume the device.
            suspend_resume_duration: How long suspend-resume operation took.
            min_buffer_duration: How much minimum buffer time to consider while
                verifying the duration.
            max_buffer_duration: How much maximum buffer time to consider while
                verifying the duration.

        Raises:
            errors.SystemPowerStateControllerError: In case of verification
                failure.
        """
        if isinstance(
            resume_mode,
            (
                system_power_state_controller_interface.AutomaticResume,
                system_power_state_controller_interface.TimerResume,
            ),
        ):
            min_expected_duration: float = (
                resume_mode.duration - min_buffer_duration
            )
            max_expected_duration: float = (
                resume_mode.duration + max_buffer_duration
            )

            if (
                suspend_resume_duration < min_expected_duration
                or suspend_resume_duration > max_expected_duration
            ):
                raise errors.SystemPowerStateControllerError(
                    f"'{suspend_state}' followed by '{resume_mode}' operation "
                    f"took {suspend_resume_duration} seconds on "
                    f"'{self._device_name}'. Expected duration range: "
                    f"[{min_expected_duration}, {max_expected_duration}] "
                    f"seconds.",
                )

    def _verify_suspend_resume_using_log_analysis(
        self,
        suspend_state: system_power_state_controller_interface.SuspendState,
        resume_mode: system_power_state_controller_interface.ResumeMode,
        logs_duration: float,
    ) -> None:
        """Verify that suspend resume operation was indeed triggered using log
        analysis.

        Args:
            suspend_state: Which state to suspend the Fuchsia device into.
            resume_mode: Information about how to resume the device.
            logs_duration: How many seconds of logs need to be captured for
                the log analysis.
        """
        duration: int = round(logs_duration) + 1
        suspend_resume_logs_cmd: list[str] = [
            "log",
            "--symbolize",
            "off",  # Turn off symbolize to run this cmd in infra
            "--filter",
            "remote-control",  # Lacewing logs
            "--filter",
            "system-activity-governor",  # suspend-resume logs
            "--since",
            f"{duration}s ago",
            "dump",
        ]

        suspend_resume_logs: list[str] = self._ffx.run(
            suspend_resume_logs_cmd,
            timeout=_Timeouts.FFX_LOGS_CMD,
        ).split("\n")

        suspend_time: float = 0
        resume_time: float = 0

        expected_patterns: list[
            re.Pattern[str]
        ] = _RegExPatterns.SUSPEND_RESUME_PATTERNS.copy()
        for suspend_resume_log in suspend_resume_logs:
            reg_ex: re.Pattern[str] = expected_patterns[0]
            match: re.Match[str] | None = reg_ex.search(suspend_resume_log)
            if match:
                if reg_ex == _RegExPatterns.SUSPEND_OPERATION:
                    suspend_time = float(match.group(1))
                elif reg_ex == _RegExPatterns.RESUME_OPERATION:
                    resume_time = float(match.group(1))
                expected_patterns.remove(reg_ex)
                if not expected_patterns:
                    break
                reg_ex = expected_patterns[0]

        if expected_patterns:
            raise errors.SystemPowerStateControllerError(
                f"Log analysis for '{suspend_state}' followed by "
                f"'{resume_mode}' operation failed on '{self._device_name}'. "
                f"Following patterns were not found: {expected_patterns}"
            )

        suspend_resume_duration: float = round(resume_time - suspend_time, 2)
        _LOGGER.info(
            "'%s' followed by '%s' operations on '%s' took %s seconds as per "
            "device logs.",
            suspend_state,
            resume_mode,
            self._device_name,
            suspend_resume_duration,
        )
        self._verify_suspend_resume_using_duration(
            suspend_state=suspend_state,
            resume_mode=resume_mode,
            suspend_resume_duration=suspend_resume_duration,
            min_buffer_duration=1,
            max_buffer_duration=0.5,
        )
