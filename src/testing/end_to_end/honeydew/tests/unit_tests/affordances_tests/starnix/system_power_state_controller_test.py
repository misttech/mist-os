#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.starnix.system_power_state_controller.py."""

import io
import subprocess
import unittest
from collections.abc import Callable
from unittest import mock

from parameterized import param, parameterized

from honeydew import errors
from honeydew.affordances.starnix import (
    system_power_state_controller as starnix_system_power_state_controller,
)
from honeydew.interfaces.affordances import (
    system_power_state_controller as system_power_state_controller_interface,
)
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import ffx as ffx_transport

_INPUT_ARGS: dict[str, object] = {
    "device_name": "fuchsia-emulator",
}

_SUSPEND_RESUME_SUCCESS_LOGS: list[str] = [
    "[00174.893370][remote-control][lacewing] INFO: [Host Time: 2024-04-16-08-38-16-PM] - Performing 'IdleSuspend' followed by 'AutomaticResume after 5sec' operations on 'fuchsia-c863-1470-a849'...",
    "[00175.010150][system-activity-governor] INFO: Suspending",
    "[00180.030013][system-activity-governor] INFO: Resuming response=Ok(Ok(SuspenderSuspendResponse { reason: None, suspend_duration: Some(0), suspend_overhead: Some(0), __source_breaking: SourceBreaking }))",
    "[00180.088070][remote-control][lacewing] INFO: [Host Time: 2024-04-16-08-38-21-PM] - Completed 'IdleSuspend' followed by 'AutomaticResume after 5sec' operations on 'fuchsia-c863-1470-a849' in 5.165762186050415 seconds.",
]

_SUSPEND_RESUME_FAILURE_LOGS_NO_LACEWING_START: list[str] = [
    "[00175.010150][system-activity-governor] INFO: Suspending",
    "[00180.030013][system-activity-governor] INFO: Resuming response=Ok(Ok(SuspenderSuspendResponse { reason: None, suspend_duration: Some(0), suspend_overhead: Some(0), __source_breaking: SourceBreaking }))",
    "[00180.088070][remote-control][lacewing] INFO: [Host Time: 2024-04-16-08-38-21-PM] - Completed 'IdleSuspend' followed by 'AutomaticResume after 5sec' operations on 'fuchsia-c863-1470-a849' in 5.165762186050415 seconds.",
]

_SUSPEND_RESUME_FAILURE_LOGS_NO_SUSPEND: list[str] = [
    "[00174.893370][remote-control][lacewing] INFO: [Host Time: 2024-04-16-08-38-16-PM] - Performing 'IdleSuspend' followed by 'AutomaticResume after 5sec' operations on 'fuchsia-c863-1470-a849'...",
    "[00180.030013][system-activity-governor] INFO: Resuming response=Ok(Ok(SuspenderSuspendResponse { reason: None, suspend_duration: Some(0), suspend_overhead: Some(0), __source_breaking: SourceBreaking }))",
    "[00180.088070][remote-control][lacewing] INFO: [Host Time: 2024-04-16-08-38-21-PM] - Completed 'IdleSuspend' followed by 'AutomaticResume after 5sec' operations on 'fuchsia-c863-1470-a849' in 5.165762186050415 seconds.",
]

_SUSPEND_RESUME_FAILURE_LOGS_NO_RESUME: list[str] = [
    "[00174.893370][remote-control][lacewing] INFO: [Host Time: 2024-04-16-08-38-16-PM] - Performing 'IdleSuspend' followed by 'AutomaticResume after 5sec' operations on 'fuchsia-c863-1470-a849'...",
    "[00175.010150][system-activity-governor] INFO: Suspending",
    "[00180.088070][remote-control][lacewing] INFO: [Host Time: 2024-04-16-08-38-21-PM] - Completed 'IdleSuspend' followed by 'AutomaticResume after 5sec' operations on 'fuchsia-c863-1470-a849' in 5.165762186050415 seconds.",
]

_SUSPEND_RESUME_FAILURE_LOGS_NO_LACEWING_END: list[str] = [
    "[00174.893370][remote-control][lacewing] INFO: [Host Time: 2024-04-16-08-38-16-PM] - Performing 'IdleSuspend' followed by 'AutomaticResume after 5sec' operations on 'fuchsia-c863-1470-a849'...",
    "[00175.010150][system-activity-governor] INFO: Suspending",
    "[00180.030013][system-activity-governor] INFO: Resuming response=Ok(Ok(SuspenderSuspendResponse { reason: None, suspend_duration: Some(0), suspend_overhead: Some(0), __source_breaking: SourceBreaking }))",
]

_HRTIMER_CTL_OUTPUT: list[str] = [
    "Executing on device /dev/class/hrtimer/4515c6a8",
    "Setting event...",
    "Starting timer...",
    "Timer started",
    "Waiting on event...",
    "Event trigged",
]

_HRTIMER_CTL_OUTPUT_NO_TIMER_START: list[str] = [
    "Executing on device /dev/class/hrtimer/4515c6a8",
    "Setting event...",
    "",
]

_HRTIMER_CTL_OUTPUT_NO_TIMER_END: list[str] = [
    "Executing on device /dev/class/hrtimer/4515c6a8",
    "Setting event...",
    "Starting timer...",
    "Timer started",
    "Waiting on event...",
]

_HRTIMER_CTL_OUTPUT_NO_TIMER_START_FOR_TIMEOUT: str = (
    "Executing on device /dev/class/hrtimer/4515c6a8"
)


def _custom_test_name_func(
    testcase_func: Callable[..., None], _: str, param_obj: param
) -> str:
    """Custom test name function method."""
    test_func_name: str = testcase_func.__name__
    test_label: str = parameterized.to_safe_name(param_obj.kwargs["label"])
    return f"{test_func_name}_with_{test_label}"


# pylint: disable=protected-access
class SystemPowerStateControllerStarnixTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.starnix.system_power_state_controller.py."""

    def setUp(self) -> None:
        super().setUp()

        self.mock_ffx = mock.MagicMock(spec=ffx_transport.FFX)
        self.mock_device_logger = mock.MagicMock(
            spec=affordances_capable.FuchsiaDeviceLogger
        )

        with mock.patch.object(
            starnix_system_power_state_controller.SystemPowerStateController,
            "_run_starnix_console_shell_cmd",
            autospec=True,
        ) as mock_run_starnix_console_shell_cmd:
            self.system_power_state_controller_obj = starnix_system_power_state_controller.SystemPowerStateController(
                ffx=self.mock_ffx,
                device_logger=self.mock_device_logger,
                device_name=str(_INPUT_ARGS["device_name"]),
            )

            mock_run_starnix_console_shell_cmd.assert_called_once()

    @mock.patch.object(
        starnix_system_power_state_controller.SystemPowerStateController,
        "suspend_resume",
        autospec=True,
    )
    def test_idle_suspend_auto_resume(
        self, mock_suspend_resume: mock.Mock
    ) -> None:
        """Test case for SystemPowerStateController.idle_suspend_auto_resume()"""

        self.system_power_state_controller_obj.idle_suspend_auto_resume(
            verify=False,
        )
        mock_suspend_resume.assert_called_once_with(
            mock.ANY,
            suspend_state=system_power_state_controller_interface.IdleSuspend(),
            resume_mode=system_power_state_controller_interface.AutomaticResume(),
            verify=False,
        )

    @mock.patch.object(
        starnix_system_power_state_controller.SystemPowerStateController,
        "suspend_resume",
        autospec=True,
    )
    def test_idle_suspend_timer_based_resume(
        self, mock_suspend_resume: mock.Mock
    ) -> None:
        """Test case for SystemPowerStateController.idle_suspend_timer_based_resume()"""

        self.system_power_state_controller_obj.idle_suspend_timer_based_resume(
            duration=3,
            verify=False,
        )
        mock_suspend_resume.assert_called_once_with(
            mock.ANY,
            suspend_state=system_power_state_controller_interface.IdleSuspend(),
            resume_mode=system_power_state_controller_interface.TimerResume(
                duration=3
            ),
            verify=False,
        )

    @mock.patch.object(
        starnix_system_power_state_controller.SystemPowerStateController,
        "_verify_suspend_resume",
        autospec=True,
    )
    @mock.patch.object(
        starnix_system_power_state_controller.SystemPowerStateController,
        "_suspend",
        autospec=True,
    )
    def test_suspend_resume_to_do_idle_suspend_auto_resume(
        self,
        mock_suspend: mock.Mock,
        mock_verify_suspend_resume: mock.Mock,
    ) -> None:
        """Test case for SystemPowerStateController.suspend_resume()"""
        self.system_power_state_controller_obj.suspend_resume(
            suspend_state=system_power_state_controller_interface.IdleSuspend(),
            resume_mode=system_power_state_controller_interface.AutomaticResume(),
            verify=True,
        )

        mock_suspend.assert_called_once_with(
            mock.ANY,
            suspend_state=system_power_state_controller_interface.IdleSuspend(),
        )
        mock_verify_suspend_resume.assert_called_once_with(
            mock.ANY,
            suspend_state=system_power_state_controller_interface.IdleSuspend(),
            resume_mode=system_power_state_controller_interface.AutomaticResume(),
            suspend_resume_execution_time=mock.ANY,
            logs_duration=mock.ANY,
        )

    @mock.patch.object(
        starnix_system_power_state_controller.SystemPowerStateController,
        "_perform_idle_suspend",
        autospec=True,
    )
    def test_suspend_with_idle_suspend(
        self,
        mock_perform_idle_suspend: mock.Mock,
    ) -> None:
        """Test case for SystemPowerStateController._suspend()"""
        self.system_power_state_controller_obj._suspend(
            suspend_state=system_power_state_controller_interface.IdleSuspend(),
        )

        mock_perform_idle_suspend.assert_called_once_with(mock.ANY)

    def test_suspend_with_not_supported_suspend_mode(self) -> None:
        """Test case for SystemPowerStateController._suspend() raising
        NotSupportedError for suspend operation."""

        with self.assertRaises(errors.NotSupportedError):
            self.system_power_state_controller_obj._suspend(
                suspend_state="invalid",  # type: ignore[arg-type]
            )

    @mock.patch.object(
        starnix_system_power_state_controller.SystemPowerStateController,
        "_run_starnix_console_shell_cmd",
        autospec=True,
    )
    def test_perform_idle_suspend(
        self,
        mock_run_starnix_console_shell_cmd: mock.Mock,
    ) -> None:
        """Test case for SystemPowerStateController._perform_idle_suspend()"""
        self.system_power_state_controller_obj._perform_idle_suspend()

        mock_run_starnix_console_shell_cmd.assert_called_once_with(
            mock.ANY,
            cmd=starnix_system_power_state_controller._StarnixCmds.IDLE_SUSPEND,
            timeout=None,
        )

    @mock.patch.object(
        starnix_system_power_state_controller.SystemPowerStateController,
        "_run_starnix_console_shell_cmd",
        side_effect=RuntimeError("Error"),
        autospec=True,
    )
    def test_perform_idle_suspend_failure(
        self,
        mock_run_starnix_console_shell_cmd: mock.Mock,
    ) -> None:
        """Test case for SystemPowerStateController._perform_idle_suspend() raising exception"""
        with self.assertRaises(errors.SystemPowerStateControllerError):
            self.system_power_state_controller_obj._perform_idle_suspend()

        mock_run_starnix_console_shell_cmd.assert_called_once_with(
            mock.ANY,
            cmd=starnix_system_power_state_controller._StarnixCmds.IDLE_SUSPEND,
            timeout=None,
        )

    def test_set_resume_mode_with_automatic_resume(self) -> None:
        """Test case for SystemPowerStateController._set_resume_mode() to
        perform automatic resume."""
        self.system_power_state_controller_obj._set_resume_mode(
            resume_mode=system_power_state_controller_interface.AutomaticResume(),
        )

    @mock.patch.object(
        starnix_system_power_state_controller.SystemPowerStateController,
        "_wait_for_timer_end",
        autospec=True,
    )
    @mock.patch.object(
        starnix_system_power_state_controller.SystemPowerStateController,
        "_wait_for_timer_start",
        autospec=True,
    )
    def test_set_resume_mode_with_timer_resume(
        self,
        mock_wait_for_timer_start: mock.Mock,
        mock_wait_for_timer_end: mock.Mock,
    ) -> None:
        """Test case for SystemPowerStateController._set_resume_mode() to
        perform timer based resume."""
        resume_mode: system_power_state_controller_interface.TimerResume = (
            system_power_state_controller_interface.TimerResume(
                duration=3,
            )
        )

        with self.system_power_state_controller_obj._set_resume_mode(
            resume_mode=resume_mode,
        ):
            pass

        mock_wait_for_timer_start.assert_called_once_with(
            mock.ANY,
            proc=mock.ANY,
        )

        mock_wait_for_timer_end.assert_called_once_with(
            mock.ANY,
            proc=mock.ANY,
            resume_mode=resume_mode,
        )

    def test_set_resume_mode_with_not_supported_resume_mode(self) -> None:
        """Test case for SystemPowerStateController._set_resume_mode() raising
        NotSupportedError for resume operation."""
        with self.assertRaises(errors.NotSupportedError):
            with self.system_power_state_controller_obj._set_resume_mode(
                resume_mode=system_power_state_controller_interface.ButtonPressResume(),
            ):
                pass

    def test_set_timer(self) -> None:
        """Test case for SystemPowerStateController._set_timer() success case."""
        self.system_power_state_controller_obj._set_timer(duration=3)

    def test_set_timer_failure(self) -> None:
        """Test case for SystemPowerStateController._set_timer() raising
        an exception."""
        self.mock_ffx.popen.side_effect = RuntimeError("Error")

        with self.assertRaises(errors.SystemPowerStateControllerError):
            self.system_power_state_controller_obj._set_timer(duration=3)

    def test_wait_for_timer_start(self) -> None:
        """Test case for SystemPowerStateController._wait_for_timer_start()
        success case."""
        mock_subprocess_popen = mock.MagicMock(spec=subprocess.Popen)
        mock_subprocess_popen.stdout = mock.MagicMock(spec=io.TextIOWrapper)
        mock_subprocess_popen.stdout.readline.side_effect = _HRTIMER_CTL_OUTPUT

        self.system_power_state_controller_obj._wait_for_timer_start(
            proc=mock_subprocess_popen,
        )

    def test_wait_for_timer_start_exception_1(self) -> None:
        """Test case for SystemPowerStateController._wait_for_timer_start()
        raising SystemPowerStateControllerError exception because of not able to
        read command output."""
        mock_subprocess_popen = mock.MagicMock(spec=subprocess.Popen)
        mock_subprocess_popen.stdout = mock.MagicMock(spec=int)

        with self.assertRaisesRegex(
            errors.SystemPowerStateControllerError,
            "Failed to read hrtimer-ctl output",
        ):
            self.system_power_state_controller_obj._wait_for_timer_start(
                proc=mock_subprocess_popen,
            )

    def test_wait_for_timer_start_exception_2(self) -> None:
        """Test case for SystemPowerStateController._wait_for_timer_start()
        raising SystemPowerStateControllerError exception because of timer
        start message was not found."""
        mock_subprocess_popen = mock.MagicMock(spec=subprocess.Popen)
        mock_subprocess_popen.stdout = mock.MagicMock(spec=io.TextIOWrapper)
        mock_subprocess_popen.stdout.readline.side_effect = (
            _HRTIMER_CTL_OUTPUT_NO_TIMER_START
        )

        with self.assertRaisesRegex(
            errors.SystemPowerStateControllerError,
            "Timer has not been started on",
        ):
            self.system_power_state_controller_obj._wait_for_timer_start(
                proc=mock_subprocess_popen,
            )

    @mock.patch("time.time", side_effect=[0, 1, 2], autospec=True)
    def test_wait_for_timer_start_timeout_exception(
        self, mock_time: mock.Mock
    ) -> None:
        """Test case for SystemPowerStateController._wait_for_timer_start()
        returning HoneydewTimeoutError exception."""
        mock_subprocess_popen = mock.MagicMock(spec=subprocess.Popen)
        mock_subprocess_popen.stdout = mock.MagicMock(spec=io.TextIOWrapper)
        mock_subprocess_popen.stdout.readline.return_value = (
            _HRTIMER_CTL_OUTPUT_NO_TIMER_START_FOR_TIMEOUT
        )

        with self.assertRaisesRegex(
            errors.HoneydewTimeoutError,
            "Timer has not been started on.*?in.*?sec",
        ):
            self.system_power_state_controller_obj._wait_for_timer_start(
                proc=mock_subprocess_popen,
            )

        mock_time.assert_called()

    def test_wait_for_timer_end(self) -> None:
        """Test case for SystemPowerStateController._wait_for_timer_end()
        success case."""
        mock_subprocess_popen = mock.MagicMock(spec=subprocess.Popen)
        mock_subprocess_popen.communicate.return_value = (
            "\n".join(_HRTIMER_CTL_OUTPUT),
            None,
        )
        mock_subprocess_popen.returncode = 0

        self.system_power_state_controller_obj._wait_for_timer_end(
            proc=mock_subprocess_popen,
            resume_mode=system_power_state_controller_interface.TimerResume(
                duration=3
            ),
        )

    def test_wait_for_timer_end_exception_1(self) -> None:
        """Test case for SystemPowerStateController._wait_for_timer_end()
        raising SystemPowerStateControllerError exception because of timer
        end message was not found."""
        mock_subprocess_popen = mock.MagicMock(spec=subprocess.Popen)
        mock_subprocess_popen.communicate.return_value = (
            "\n".join(_HRTIMER_CTL_OUTPUT_NO_TIMER_END),
            None,
        )
        mock_subprocess_popen.returncode = 0

        with self.assertRaisesRegex(
            errors.SystemPowerStateControllerError,
            "hrtimer-ctl completed without ending the timer",
        ):
            self.system_power_state_controller_obj._wait_for_timer_end(
                proc=mock_subprocess_popen,
                resume_mode=system_power_state_controller_interface.TimerResume(
                    duration=3
                ),
            )

    def test_wait_for_timer_end_exception_2(self) -> None:
        """Test case for SystemPowerStateController._wait_for_timer_end()
        raising SystemPowerStateControllerError exception because of failed to
        read command output."""
        mock_subprocess_popen = mock.MagicMock(spec=subprocess.Popen)
        mock_subprocess_popen.communicate.return_value = (
            None,
            "Mock Error",
        )
        mock_subprocess_popen.returncode = 1

        with self.assertRaisesRegex(
            errors.SystemPowerStateControllerError,
            "hrtimer-ctl returned a failure while waiting for the timer to "
            "end. returncode=1.*?error='Mock Error'",
        ):
            self.system_power_state_controller_obj._wait_for_timer_end(
                proc=mock_subprocess_popen,
                resume_mode=system_power_state_controller_interface.TimerResume(
                    duration=3
                ),
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
        self, mock_openpty: mock.Mock, mock_os_read: mock.Mock
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
        self, mock_openpty: mock.Mock, mock_os_read: mock.Mock
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
        self, mock_openpty: mock.Mock, mock_os_read: mock.Mock
    ) -> None:
        """Test case for SystemPowerStateController._run_starnix_console_shell_cmd()
        raising StarnixError"""
        with self.assertRaises(errors.StarnixError):
            self.system_power_state_controller_obj._run_starnix_console_shell_cmd(
                cmd=["something"]
            )
        mock_openpty.assert_called_once()
        mock_os_read.assert_called_once()

    @mock.patch.object(
        starnix_system_power_state_controller.SystemPowerStateController,
        "_verify_suspend_resume_using_log_analysis",
        autospec=True,
    )
    @mock.patch.object(
        starnix_system_power_state_controller.SystemPowerStateController,
        "_verify_suspend_resume_using_duration",
        autospec=True,
    )
    def test_verify_suspend_resume(
        self,
        mock_verify_suspend_resume_using_duration: mock.Mock,
        mock_verify_suspend_resume_using_log_analysis: mock.Mock,
    ) -> None:
        """Test case for SystemPowerStateController._verify_suspend_resume()"""
        self.system_power_state_controller_obj._verify_suspend_resume(
            suspend_state=system_power_state_controller_interface.IdleSuspend(),
            resume_mode=system_power_state_controller_interface.AutomaticResume(),
            suspend_resume_execution_time=5,
            logs_duration=7,
        )

        mock_verify_suspend_resume_using_duration.assert_called_once_with(
            mock.ANY,
            suspend_state=system_power_state_controller_interface.IdleSuspend(),
            resume_mode=system_power_state_controller_interface.AutomaticResume(),
            suspend_resume_duration=5,
            min_buffer_duration=0,
            max_buffer_duration=3,
        )

        mock_verify_suspend_resume_using_log_analysis.assert_called_once_with(
            mock.ANY,
            suspend_state=system_power_state_controller_interface.IdleSuspend(),
            resume_mode=system_power_state_controller_interface.AutomaticResume(),
            logs_duration=7,
        )

    def test_verify_suspend_resume_using_duration_success(self) -> None:
        """Test case for SystemPowerStateController._verify_suspend_resume_using_duration()
        success case"""
        self.system_power_state_controller_obj._verify_suspend_resume_using_duration(
            suspend_state=system_power_state_controller_interface.IdleSuspend(),
            resume_mode=system_power_state_controller_interface.AutomaticResume(),
            suspend_resume_duration=system_power_state_controller_interface.AutomaticResume.duration
            + 2,
            min_buffer_duration=0,
            max_buffer_duration=2,
        )

    def test_verify_suspend_resume_using_duration_fail(self) -> None:
        """Test case for SystemPowerStateController._verify_suspend_resume_using_duration()
        failure case"""
        with self.assertRaisesRegex(
            errors.SystemPowerStateControllerError,
            "'IdleSuspend' followed by 'AutomaticResume after .+sec' "
            "operation took .+ seconds on 'fuchsia-emulator'",
        ):
            self.system_power_state_controller_obj._verify_suspend_resume_using_duration(
                suspend_state=system_power_state_controller_interface.IdleSuspend(),
                resume_mode=system_power_state_controller_interface.AutomaticResume(),
                suspend_resume_duration=system_power_state_controller_interface.AutomaticResume.duration
                + 20,
                min_buffer_duration=1,
                max_buffer_duration=1,
            )

    def test_verify_suspend_resume_using_log_analysis_success(self) -> None:
        """Test case for SystemPowerStateController._verify_suspend_resume_using_log_analysis()
        success case"""
        self.mock_ffx.run.return_value = "\n".join(_SUSPEND_RESUME_SUCCESS_LOGS)

        self.system_power_state_controller_obj._verify_suspend_resume_using_log_analysis(
            suspend_state=system_power_state_controller_interface.IdleSuspend(),
            resume_mode=system_power_state_controller_interface.AutomaticResume(),
            logs_duration=7,
        )

        self.mock_ffx.run.assert_called_once()

    @parameterized.expand(
        [
            param(
                label="no_suspend",
                device_logs=_SUSPEND_RESUME_FAILURE_LOGS_NO_SUSPEND,
            ),
            param(
                label="no_resume",
                device_logs=_SUSPEND_RESUME_FAILURE_LOGS_NO_RESUME,
            ),
            param(
                label="no_lacewing_start",
                device_logs=_SUSPEND_RESUME_FAILURE_LOGS_NO_LACEWING_START,
            ),
            param(
                label="no_lacewing_end",
                device_logs=_SUSPEND_RESUME_FAILURE_LOGS_NO_LACEWING_END,
            ),
        ],
        name_func=_custom_test_name_func,
    )
    def test_verify_suspend_resume_using_log_analysis_fail(
        self,
        label: str,  # pylint: disable=unused-argument
        device_logs: list[str],
    ) -> None:
        """Test case for SystemPowerStateController._verify_suspend_resume_using_log_analysis()
        failure case"""
        self.mock_ffx.run.return_value = "\n".join(device_logs)

        with self.assertRaisesRegex(
            errors.SystemPowerStateControllerError,
            "Log analysis for 'IdleSuspend' followed by 'AutomaticResume after .+sec' "
            "operation failed on 'fuchsia-emulator'",
        ):
            self.system_power_state_controller_obj._verify_suspend_resume_using_log_analysis(
                suspend_state=system_power_state_controller_interface.IdleSuspend(),
                resume_mode=system_power_state_controller_interface.AutomaticResume(),
                logs_duration=5,
            )

        self.mock_ffx.run.assert_called_once()
