#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""ABC with methods for Host-(Fuchsia)Target interactions via FFX."""

import abc
import subprocess
from typing import Any

from honeydew.typing import custom_types
from honeydew.typing import ffx as ffx_types
from honeydew.utils import properties

TIMEOUTS: dict[str, float] = {
    "FFX_CLI": 10,
    "TARGET_ADD": 60,  # TODO(https://fxbug.dev/336608577): Increasing to 60sec as a workaround for this issue
    "TARGET_RCS_CONNECTION_WAIT": 15,
    "TARGET_RCS_DISCONNECTION_WAIT": 15,
}


class FFX(abc.ABC):
    """ABC with methods for Host-(Fuchsia)Target interactions via FFX."""

    @properties.PersistentProperty
    @abc.abstractmethod
    def config(self) -> custom_types.FFXConfig:
        """Returns the FFX configuration associated with this instance of FFX
        object.

        Returns:
            custom_types.FFXConfig
        """

    @abc.abstractmethod
    def add_target(
        self,
        timeout: float = TIMEOUTS["TARGET_ADD"],
    ) -> None:
        """Adds a target to the ffx collection

        Args:
            timeout: How long in seconds to wait for FFX command to complete.

        Raises:
            errors.DeviceNotConnectedError: If FFX fails to reach target.
            errors.FfxTimeoutError: In case of FFX command timeout.
            errors.FfxCommandError: In case of other FFX command failure.
        """

    @abc.abstractmethod
    def check_connection(
        self, timeout: float = TIMEOUTS["TARGET_RCS_CONNECTION_WAIT"]
    ) -> None:
        """Checks the FFX connection from host to Fuchsia device.

        Args:
            timeout: How long in seconds to wait for FFX to establish the RCS
                connection.

        Raises:
            errors.FfxConnectionError
        """

    @abc.abstractmethod
    def get_target_information(
        self, timeout: float = TIMEOUTS["FFX_CLI"]
    ) -> ffx_types.TargetInfoData:
        """Executed and returns the output of `ffx -t {target} target show`.

        Args:
            timeout: Timeout to wait for the ffx command to return.

        Returns:
            Output of `ffx -t {target} target show`.

        Raises:
            errors.DeviceNotConnectedError: If FFX fails to reach target.
            errors.FfxTimeoutError: In case of FFX command timeout.
            errors.FfxCommandError: In case of other FFX command failure.
        """

    @abc.abstractmethod
    def get_target_info_from_target_list(
        self, timeout: float = TIMEOUTS["FFX_CLI"]
    ) -> dict[str, Any]:
        """Executed and returns the output of
        `ffx --machine json target list <target>`.

        Args:
            timeout: Timeout to wait for the ffx command to return.

        Returns:
            Output of `ffx --machine json target list <target>`.

        Raises:
            errors.FfxTimeoutError: In case of FFX command timeout.
            errors.FfxCommandError: In case of FFX command failure.
        """

    @abc.abstractmethod
    def get_target_name(self, timeout: float = TIMEOUTS["FFX_CLI"]) -> str:
        """Returns the target name.

        Args:
            timeout: Timeout to wait for the ffx command to return.

        Returns:
            Target name.

        Raises:
            errors.DeviceNotConnectedError: If FFX fails to reach target.
            errors.FfxTimeoutError: In case of FFX command timeout.
            errors.FfxCommandError: In case of other FFX command failure.
        """

    @abc.abstractmethod
    def get_target_ssh_address(
        self, timeout: float | None = TIMEOUTS["FFX_CLI"]
    ) -> custom_types.TargetSshAddress:
        """Returns the target's ssh ip address and port information.

        Args:
            timeout: Timeout to wait for the ffx command to return.

        Returns:
            (Target SSH IP Address, Target SSH Port)

        Raises:
            errors.DeviceNotConnectedError: If FFX fails to reach target.
            errors.FfxTimeoutError: In case of FFX command timeout.
            errors.FfxCommandError: In case of other FFX command failure.
        """

    @abc.abstractmethod
    def get_target_board(self, timeout: float = TIMEOUTS["FFX_CLI"]) -> str:
        """Returns the target's board.

        Args:
            timeout: Timeout to wait for the ffx command to return.

        Returns:
            Target's board.

        Raises:
            errors.DeviceNotConnectedError: If FFX fails to reach target.
            errors.FfxTimeoutError: In case of FFX command timeout.
            errors.FfxCommandError: In case of other FFX command failure.
        """

    @abc.abstractmethod
    def get_target_product(self, timeout: float = TIMEOUTS["FFX_CLI"]) -> str:
        """Returns the target's product.

        Args:
            timeout: Timeout to wait for the ffx command to return.

        Returns:
            Target's product.

        Raises:
            errors.DeviceNotConnectedError: If FFX fails to reach target.
            errors.FfxTimeoutError: In case of FFX command timeout.
            errors.FfxCommandError: In case of other FFX command failure.
        """

    @abc.abstractmethod
    def run(
        self,
        cmd: list[str],
        timeout: float | None = TIMEOUTS["FFX_CLI"],
        capture_output: bool = True,
        log_output: bool = True,
    ) -> str:
        """Runs an FFX command.

        Args:
            cmd: FFX command to run.
            timeout: Timeout to wait for the ffx command to return.
            capture_output: When True, the stdout/err from the command will be
                captured and returned. When False, the output of the command
                will be streamed to stdout/err accordingly and it won't be
                returned. Defaults to True.
            log_output: When True, logs the output in DEBUG level. Callers
                may set this to False when expecting particularly large
                or spammy output.
            include_target: If set to True, `ffx -t {target} {cmd}` will be run.
                Otherwise, `ffx {cmd}` will be run.

        Returns:
            Output of FFX command when capture_output is set to True, otherwise
            an empty string.

        Raises:
            errors.DeviceNotConnectedError: If FFX fails to reach target.
            errors.FfxTimeoutError: In case of FFX command timeout.
            errors.FfxCommandError: In case of other FFX command failure.
        """

    @abc.abstractmethod
    def popen(  # type: ignore[no-untyped-def]
        self,
        cmd: list[str],
        **kwargs,
    ) -> subprocess.Popen[custom_types.AnyString]:
        """Starts a new process to run the FFX cmd and returns the corresponding
        process.

        Intended for executing daemons or processing streamed output. Given
        the raw nature of this API, it is up to callers to detect and handle
        potential errors, and make sure to close this process eventually
        (e.g. with `popen.terminate` method). Otherwise, use the simpler `run`
        method instead.

        Args:
            cmd: FFX command to run.
            kwargs: Forwarded as-is to subprocess.Popen.

        Returns:
            The Popen object of `ffx -t {target} {cmd}`.
            If text arg of subprocess.Popen is set to True,
            subprocess.Popen[str] will be returned. Otherwise,
            subprocess.Popen[bytes] will be returned.
        """

    @abc.abstractmethod
    def run_test_component(
        self,
        component_url: str,
        ffx_test_args: list[str] | None = None,
        test_component_args: list[str] | None = None,
        timeout: float | None = TIMEOUTS["FFX_CLI"],
        capture_output: bool = True,
    ) -> str:
        """Executes and returns the output of
        `ffx -t {target} test run {component_url}` with the given options.

        This results in an invocation:
        ```
        ffx -t {target} test {component_url} {ffx_test_args} -- {test_component_args}`.
        ```

        For example:

        ```
        ffx -t fuchsia-emulator test \\
            fuchsia-pkg://fuchsia.com/my_benchmark#test.cm \\
            --output_directory /tmp \\
            -- /custom_artifacts/results.fuchsiaperf.json
        ```

        Args:
            component_url: The URL of the test to run.
            ffx_test_args: args to pass to `ffx test run`.
            test_component_args: args to pass to the test component.
            timeout: Timeout to wait for the ffx command to return.
            capture_output: When True, the stdout/err from the command will be captured and
                returned. When False, the output of the command will be streamed to stdout/err
                accordingly and it won't be returned. Defaults to True.

        Returns:
            Output of `ffx -t {target} {cmd}` when capture_output is set to True, otherwise an
            empty string.

        Raises:
            errors.DeviceNotConnectedError: If FFX fails to reach target.
            errors.FfxTimeoutError: In case of FFX command timeout.
            errors.FfxCommandError: In case of other FFX command failure.
        """

    @abc.abstractmethod
    def run_ssh_cmd(
        self,
        cmd: str,
        timeout: float | None = TIMEOUTS["FFX_CLI"],
        capture_output: bool = True,
    ) -> str:
        """Executes and returns the output of `ffx -t target ssh <cmd>`.

        Args:
            cmd: SSH command to run.
            timeout: Timeout to wait for the ffx command to return.
            capture_output: When True, the stdout/err from the command will be
                captured and returned. When False, the output of the command
                will be streamed to stdout/err accordingly and it won't be
                returned. Defaults to True.

        Returns:
            Output of `ffx -t target ssh <cmd>` when capture_output is set to
            True, otherwise an empty string.

        Raises:
            errors.DeviceNotConnectedError: If FFX fails to reach target.
            errors.FfxTimeoutError: In case of FFX command timeout.
            errors.FfxCommandError: In case of other FFX command failure.
        """

    @abc.abstractmethod
    def wait_for_rcs_connection(
        self, timeout: float = TIMEOUTS["TARGET_RCS_CONNECTION_WAIT"]
    ) -> None:
        """Wait until FFX is able to establish a RCS connection to the target.

        Args:
            timeout: How long in seconds to wait for FFX to establish the RCS
                connection.

        Raises:
            errors.DeviceNotConnectedError: If FFX fails to reach target.
            errors.FfxTimeoutError: In case of FFX command timeout.
            errors.FfxCommandError: In case of other FFX command failure.
        """

    @abc.abstractmethod
    def wait_for_rcs_disconnection(
        self, timeout: float = TIMEOUTS["TARGET_RCS_DISCONNECTION_WAIT"]
    ) -> None:
        """Wait until FFX is able to disconnect RCS connection to the target.

        Args:
            timeout: How long in seconds to wait for disconnection.

        Raises:
            errors.DeviceNotConnectedError: If FFX fails to reach target.
            errors.FfxTimeoutError: In case of FFX command timeout.
            errors.FfxCommandError: In case of other FFX command failure.
        """

    @abc.abstractmethod
    def generate_ffx_cmd(
        self,
        cmd: list[str],
        include_target: bool = True,
    ) -> list[str]:
        """Generates the FFX command that need to be run.

        Args:
            cmd: FFX command.
            include_target: True to include "-t <target_name>", False otherwise.

        Returns:
            FFX command to be run as list of string.
        """
