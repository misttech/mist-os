#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Provides methods for Host-(Fuchsia)Target interactions via FFX."""

import atexit
import ipaddress
import json
import logging
import subprocess
from collections.abc import Iterable
from typing import Any

import fuchsia_controller_py as fuchsia_controller

from honeydew import errors
from honeydew.interfaces.transports import ffx as ffx_interface
from honeydew.typing import custom_types
from honeydew.typing.ffx import TargetInfoData
from honeydew.utils import properties

_FFX_BINARY: str = "ffx"

_FFX_CONFIG_CMDS: dict[str, list[str]] = {
    "LOG_DIR": [
        "config",
        "set",
        "log.dir",
    ],
    "LOG_LEVEL": [
        "config",
        "set",
        "log.level",
    ],
    "MDNS": [
        "config",
        "set",
        "discovery.mdns.enabled",
    ],
    "SUB_TOOLS_PATH": [
        "config",
        "set",
        "ffx.subtool-search-paths",
    ],
    "DAEMON_ECHO": [
        "daemon",
        "echo",
    ],
}

_FFX_CMDS: dict[str, list[str]] = {
    "TARGET_ADD": ["target", "add"],
    "TARGET_SHOW": ["--machine", "json", "target", "show"],
    "TARGET_SSH_ADDRESS": ["target", "get-ssh-address"],
    "TARGET_LIST": ["--machine", "json", "target", "list"],
    "TARGET_WAIT": ["target", "wait", "--timeout"],
    "TARGET_WAIT_DOWN": ["target", "wait", "--down", "--timeout"],
    "TEST_RUN": ["test", "run"],
    "TARGET_SSH": ["target", "ssh"],
}

_LOGGER: logging.Logger = logging.getLogger(__name__)

_FFX_LOGS_LEVEL: str = "debug"

_DEVICE_NOT_CONNECTED: str = "Timeout attempting to reach target"


class FfxConfig:
    """Provides methods to configure FFX."""

    def __init__(self) -> None:
        self._setup_done: bool = False

    def setup(
        self,
        binary_path: str | None,
        isolate_dir: str | None,
        logs_dir: str,
        logs_level: str | None,
        enable_mdns: bool,
        subtools_search_path: str | None,
    ) -> None:
        """Sets up configuration need to be used while running FFX command.

        Args:
            binary_path: absolute path to the FFX binary.
            isolate_dir: Directory that will be passed to `--isolate-dir`
                arg of FFX
            logs_dir: Directory that will be passed to `--config log.dir`
                arg of FFX
            logs_level: logs level that will be passed to `--config log.level`
                arg of FFX
            enable_mdns: Whether or not mdns need to be enabled. This will be
                passed to `--config discovery.mdns.enabled` arg of FFX
            subtools_search_path: A path of where ffx should
                look for plugins.

        Raises:
            errors.FfxConfigError: If setup has already been called once.

        Note:
            * This method should be called only once to ensure daemon logs are
              going to single location.
            * If this method is not called then FFX logs will not be saved and
              will use the system level FFX daemon (instead of spawning new one
              using isolation).
            * FFX daemon clean up is already handled by this method though users
              can manually call close() to clean up earlier in the process if
              necessary.
        """
        if self._setup_done:
            raise errors.FfxConfigError("setup has already been called once.")

        # Prevent FFX daemon leaks by ensuring clean up occurs upon normal
        # program termination.
        atexit.register(self._atexit_callback)

        self._ffx_binary: str = binary_path if binary_path else _FFX_BINARY
        self._isolate_dir: fuchsia_controller.IsolateDir = (
            fuchsia_controller.IsolateDir(isolate_dir)
        )

        self._logs_dir: str = logs_dir
        self._logs_level: str = logs_level if logs_level else _FFX_LOGS_LEVEL
        self._mdns_enabled: bool = enable_mdns
        self.subtools_search_path: str | None = subtools_search_path

        self._run(_FFX_CONFIG_CMDS["LOG_DIR"] + [self._logs_dir])
        self._run(_FFX_CONFIG_CMDS["LOG_LEVEL"] + [self._logs_level])
        self._run(_FFX_CONFIG_CMDS["MDNS"] + [str(self._mdns_enabled).lower()])
        if self.subtools_search_path:
            self._run(
                _FFX_CONFIG_CMDS["SUB_TOOLS_PATH"] + [self.subtools_search_path]
            )

        self._run(_FFX_CONFIG_CMDS["DAEMON_ECHO"])

        self._setup_done = True

    def close(self) -> None:
        """Clean up method.

        Raises:
            errors.FfxConfigError: When called before calling `FfxConfig.setup`
        """
        if self._setup_done is False:
            raise errors.FfxConfigError("close called before calling setup.")

        # Setting to None will delete the `self._isolate_dir.directory()`
        self._isolate_dir = None

        self._setup_done = False

    def get_config(self) -> custom_types.FFXConfig:
        """Returns the FFX configuration information that has been set.

        Returns:
            custom_types.FFXConfig

        Raises:
            errors.FfxConfigError: When called before calling `FfxConfig.setup`
        """
        if self._setup_done is False:
            raise errors.FfxConfigError(
                "get_config called before calling setup."
            )

        return custom_types.FFXConfig(
            binary_path=self._ffx_binary,
            isolate_dir=self._isolate_dir,
            logs_dir=self._logs_dir,
            logs_level=self._logs_level,
            mdns_enabled=self._mdns_enabled,
            subtools_search_path=self.subtools_search_path,
        )

    def _atexit_callback(self) -> None:
        try:
            self.close()
        except errors.FfxConfigError:
            pass

    def _run(
        self,
        cmd: list[str],
        timeout: float | None = ffx_interface.TIMEOUTS["FFX_CLI"],
    ) -> None:
        """Executes `ffx {cmd}`.

        Args:
            cmd: FFX command to run.
            timeout: Timeout to wait for the ffx command to return.

        Raises:
            errors.FfxTimeoutError: In case of FFX command timeout.
            errors.FfxConfigError: In case of any other FFX command failure.
        """
        ffx_args: list[str] = []
        ffx_args.extend(["--isolate-dir", self._isolate_dir.directory()])
        ffx_cmd: list[str] = [self._ffx_binary] + ffx_args + cmd

        try:
            _LOGGER.debug("Executing command `%s`", " ".join(ffx_cmd))
            subprocess.check_call(ffx_cmd, timeout=timeout)
            _LOGGER.debug("`%s` finished executing", " ".join(ffx_cmd))
            return
        except subprocess.TimeoutExpired as err:
            raise errors.FfxTimeoutError(
                f"`{cmd} timed out after {timeout}sec."
            ) from err
        except subprocess.CalledProcessError as err:
            message: str = (
                f"Command '{ffx_cmd}' failed. returncode = {err.returncode}"
            )
            if err.stdout:
                message += f", stdout = {err.stdout}"
            if err.stderr:
                message += f", stderr = {err.stderr}."
            _LOGGER.debug(message)

            raise errors.FfxConfigError(f"`{cmd}` command failed") from err


class FFX(ffx_interface.FFX):
    """Provides methods for Host-(Fuchsia)Target interactions via FFX.

    Args:
        target_name: Fuchsia device name.
        config: Configuration associated with FFX and FFX daemon.
        target_ip_port: Fuchsia device IP address and port.

        Note: When target_ip is provided, it will be used instead of target_name
        while running ffx commands (ex: `ffx -t <target_ip> <command>`).

    Raises:
        errors.FfxConnectionError: In case of failed to check FFX connection.
        errors.FfxTimeoutError: In case of FFX commands timeout.
        errors.FfxCommandError: In case of failure.
    """

    def __init__(
        self,
        target_name: str,
        config: custom_types.FFXConfig,
        target_ip_port: custom_types.IpPort | None = None,
    ) -> None:
        invalid_target_name: bool = False
        try:
            ipaddress.ip_address(target_name)
            invalid_target_name = True
        except ValueError:
            pass
        if invalid_target_name:
            raise ValueError(
                f"{target_name=} is an IP address instead of target name"
            )

        self._config: custom_types.FFXConfig = config

        self._target_name: str = target_name

        self._target_ip_port: custom_types.IpPort | None = target_ip_port

        self._target: str
        if self._target_ip_port:
            self._target = str(self._target_ip_port)
        else:
            self._target = self._target_name

        if self._target_ip_port:
            self.add_target()
        self.check_connection()

    @properties.PersistentProperty
    def config(self) -> custom_types.FFXConfig:
        """Returns the FFX configuration associated with this instance of FFX
        object.

        Returns:
            custom_types.FFXConfig
        """
        return self._config

    def add_target(
        self,
        timeout: float = ffx_interface.TIMEOUTS["TARGET_ADD"],
    ) -> None:
        """Adds a target to the ffx collection

        Args:
            timeout: How long in seconds to wait for FFX command to complete.

        Raises:
            errors.FfxTimeoutError: In case of timeout.
            errors.FfxCommandError: In case of failure.
        """
        cmd: list[str] = _FFX_CMDS["TARGET_ADD"] + [str(self._target_ip_port)]
        ffx_cmd: list[str] = self._generate_ffx_cmd(
            cmd=cmd, include_target=False
        )

        try:
            _LOGGER.debug("Adding target '%s'", self._target_ip_port)
            _LOGGER.debug("Executing command `%s`", " ".join(ffx_cmd))
            output: str = subprocess.check_output(
                ffx_cmd, stderr=subprocess.STDOUT, timeout=timeout
            ).decode()
            _LOGGER.debug("`%s` returned: %s", " ".join(ffx_cmd), output)
        except subprocess.TimeoutExpired as err:
            raise errors.FfxTimeoutError(
                f"`{cmd} command timed out after {timeout}sec."
            ) from err
        except subprocess.CalledProcessError as err:
            message: str = (
                f"Command '{ffx_cmd}' failed. returncode = {err.returncode}"
            )
            if err.stdout:
                message += f", stdout = {err.stdout}"
            if err.stderr:
                message += f", stderr = {err.stderr}."
            _LOGGER.debug(message)

            raise errors.FfxCommandError(f"`{cmd}` command failed") from err

        except Exception as err:  # pylint: disable=broad-except
            raise errors.FfxCommandError(f"`{cmd}` command failed") from err

    def check_connection(
        self,
        timeout: float = ffx_interface.TIMEOUTS["TARGET_RCS_CONNECTION_WAIT"],
    ) -> None:
        """Checks the FFX connection from host to Fuchsia device.

        Args:
            timeout: How long in seconds to wait for FFX to establish the RCS
                connection.

        Raises:
            errors.FfxConnectionError
        """
        try:
            self.wait_for_rcs_connection(timeout=timeout)
        except Exception as err:  # pylint: disable=broad-except
            raise errors.FfxConnectionError(
                f"FFX connection check failed for {self._target_name} with err: {err}"
            ) from err

    def get_target_information(
        self, timeout: float = ffx_interface.TIMEOUTS["FFX_CLI"]
    ) -> TargetInfoData:
        """Executed and returns the output of `ffx -t {target} target show`.

        Args:
            timeout: Timeout to wait for the ffx command to return.

        Returns:
            Output of `ffx -t {target} target show`.

        Raises:
            FfxTimeoutError: In case of timeout.
            errors.FfxCommandError: In case of failure.
        """
        cmd: list[str] = _FFX_CMDS["TARGET_SHOW"]
        try:
            output: str = self.run(cmd=cmd, timeout=timeout)

            target_info = TargetInfoData(**json.loads(output))
            _LOGGER.debug("`%s` returned: %s", " ".join(cmd), target_info)

            return target_info
        except subprocess.TimeoutExpired as err:
            raise errors.FfxTimeoutError(
                f"`{cmd} command timed out after {timeout}sec."
            ) from err
        except Exception as err:  # pylint: disable=broad-except
            raise errors.FfxCommandError(
                f"Failed to get the target information of {self._target_name}"
            ) from err

    def get_target_list(
        self, timeout: float = ffx_interface.TIMEOUTS["FFX_CLI"]
    ) -> list[dict[str, Any]]:
        """Executed and returns the output of `ffx --machine json target list`.

        Args:
            timeout: Timeout to wait for the ffx command to return.

        Returns:
            Output of `ffx --machine json target list`.

        Raises:
            errors.FfxCommandError: In case of failure.
        """
        cmd: list[str] = _FFX_CMDS["TARGET_LIST"]
        try:
            output: str = self.run(cmd=cmd, timeout=timeout)

            ffx_target_list_info: list[dict[str, Any]] = json.loads(output)
            _LOGGER.debug(
                "`%s` returned: %s", " ".join(cmd), ffx_target_list_info
            )

            return ffx_target_list_info
        except Exception as err:  # pylint: disable=broad-except
            raise errors.FfxCommandError(f"`{cmd}` command failed") from err

    def get_target_name(
        self, timeout: float = ffx_interface.TIMEOUTS["FFX_CLI"]
    ) -> str:
        """Returns the target name.

        Args:
            timeout: Timeout to wait for the ffx command to return.

        Returns:
            Target name.

        Raises:
            errors.FfxCommandError: In case of failure.
        """
        try:
            ffx_target_show_info: TargetInfoData = self.get_target_information(
                timeout
            )
            return ffx_target_show_info.target.name
        except Exception as err:  # pylint: disable=broad-except
            raise errors.FfxCommandError(
                f"Failed to get the target name of {self._target_name}"
            ) from err

    def get_target_ssh_address(
        self, timeout: float | None = ffx_interface.TIMEOUTS["FFX_CLI"]
    ) -> custom_types.TargetSshAddress:
        """Returns the target's ssh ip address and port information.

        Args:
            timeout: Timeout to wait for the ffx command to return.

        Returns:
            (Target SSH IP Address, Target SSH Port)

        Raises:
            errors.FfxCommandError: In case of failure.
        """
        cmd: list[str] = _FFX_CMDS["TARGET_SSH_ADDRESS"]
        try:
            output: str = self.run(cmd=cmd, timeout=timeout)
            output = output.strip()
            _LOGGER.debug("`%s` returned: %s", " ".join(cmd), output)

            # in '[fe80::6a47:a931:1e84:5077%qemu]:22', ":22" is SSH port.
            # Ports can be 1-5 chars, clip off everything after the last ':'.
            ssh_info: list[str] = output.rsplit(":", 1)
            ssh_ip: str = ssh_info[0].replace("[", "").replace("]", "")
            ssh_port: int = int(ssh_info[1])

            return custom_types.TargetSshAddress(
                ip=ipaddress.ip_address(ssh_ip), port=ssh_port
            )
        except Exception as err:  # pylint: disable=broad-except
            raise errors.FfxCommandError(
                f"Failed to get the SSH address of {self._target_name}"
            ) from err

    def get_target_board(
        self, timeout: float = ffx_interface.TIMEOUTS["FFX_CLI"]
    ) -> str:
        """Returns the target's board.

        Args:
            timeout: Timeout to wait for the ffx command to return.

        Returns:
            Target's board.

        Raises:
            errors.FfxCommandError: In case of failure.
        """
        target_show_info: TargetInfoData = self.get_target_information(
            timeout=timeout
        )
        return (
            target_show_info.build.board if target_show_info.build.board else ""
        )

    def get_target_product(
        self, timeout: float = ffx_interface.TIMEOUTS["FFX_CLI"]
    ) -> str:
        """Returns the target's product.

        Args:
            timeout: Timeout to wait for the ffx command to return.

        Returns:
            Target's product.

        Raises:
            errors.FfxCommandError: In case of failure.
        """
        target_show_info: TargetInfoData = self.get_target_information(
            timeout=timeout
        )
        return (
            target_show_info.build.product
            if target_show_info.build.product
            else ""
        )

    # pylint: disable=missing-raises-doc
    # To handle below pylint warning:
    #   W9006: "Exception" not documented as being raised (missing-raises-doc)
    def run(
        self,
        cmd: list[str],
        timeout: float | None = ffx_interface.TIMEOUTS["FFX_CLI"],
        exceptions_to_skip: Iterable[type[Exception]] | None = None,
        capture_output: bool = True,
        log_output: bool = True,
    ) -> str:
        """Executes and returns the output of `ffx -t {target} {cmd}`.

        Args:
            cmd: FFX command to run.
            timeout: Timeout to wait for the ffx command to return.
            exceptions_to_skip: Any non fatal exceptions to be ignored.
            capture_output: When True, the stdout/err from the command will be
                captured and returned. When False, the output of the command
                will be streamed to stdout/err accordingly and it won't be
                returned. Defaults to True.
            log_output: When True, logs the output in DEBUG level. Callers
                may set this to False when expecting particularly large
                or spammy output.

        Returns:
            Output of `ffx -t {target} {cmd}` when capture_output is set to True, otherwise an
            empty string.

        Raises:
            errors.DeviceNotConnectedError: If FFX fails to reach target.
            errors.FfxTimeoutError: In case of FFX command timeout.
            errors.FfxCommandError: In case of other FFX command failure.
        """
        exceptions_to_skip = tuple(exceptions_to_skip or [])

        ffx_cmd: list[str] = self._generate_ffx_cmd(cmd=cmd)
        try:
            _LOGGER.debug("Executing command `%s`", " ".join(ffx_cmd))
            if capture_output:
                output: str = subprocess.check_output(
                    ffx_cmd, stderr=subprocess.PIPE, timeout=timeout
                ).decode()
                if log_output:
                    _LOGGER.debug(
                        "`%s` returned: %s", " ".join(ffx_cmd), output
                    )
                return output
            else:
                subprocess.check_call(ffx_cmd, timeout=timeout)
                _LOGGER.debug("`%s` finished executing", " ".join(ffx_cmd))
                return ""
        except Exception as err:  # pylint: disable=broad-except
            # Catching all exceptions into this broad one because of
            # `exceptions_to_skip` argument

            if isinstance(err, exceptions_to_skip):
                return ""

            if isinstance(err, subprocess.TimeoutExpired):
                raise errors.FfxTimeoutError(
                    f"`{cmd} command timed out after {timeout}sec."
                )

            if isinstance(err, subprocess.CalledProcessError):
                message: str = (
                    f"Command '{ffx_cmd}' failed. returncode = {err.returncode}"
                )
                if err.stdout:
                    message += f", stdout = {err.stdout}"
                if err.stderr:
                    message += f", stderr = {err.stderr}."
                _LOGGER.debug(message)

                if _DEVICE_NOT_CONNECTED in str(err.output):
                    raise errors.DeviceNotConnectedError(
                        f"{self._target_name} is not connected to host"
                    ) from err

            raise errors.FfxCommandError(f"`{cmd}` command failed") from err

    # pylint: enable=missing-raises-doc

    def popen(  # type: ignore[no-untyped-def]
        self,
        cmd: list[str],
        **kwargs,
    ) -> subprocess.Popen[Any]:
        """Executes the command `ffx -t {target} ... {cmd}` via `subprocess.Popen`.

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

        Raises:
            errors.FfxCommandError: On failure.
        """
        ffx_cmd: list[str] = self._generate_ffx_cmd(cmd=cmd)
        _LOGGER.info("Opening ffx process `%s`...", " ".join(ffx_cmd))
        try:
            return subprocess.Popen(ffx_cmd, **kwargs)
        except Exception as err:  # pylint: disable=broad-except
            raise errors.FfxCommandError(f"`{cmd}` command failed") from err

    def run_test_component(
        self,
        component_url: str,
        ffx_test_args: list[str] | None = None,
        test_component_args: list[str] | None = None,
        timeout: float | None = ffx_interface.TIMEOUTS["FFX_CLI"],
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
        cmd: list[str] = _FFX_CMDS["TEST_RUN"][:]
        cmd.append(component_url)
        if ffx_test_args:
            cmd += ffx_test_args
        if test_component_args:
            cmd.append("--")
            cmd += test_component_args
        return self.run(cmd, timeout=timeout, capture_output=capture_output)

    def run_ssh_cmd(
        self,
        cmd: str,
        timeout: float | None = ffx_interface.TIMEOUTS["FFX_CLI"],
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
        ffx_cmd: list[str] = _FFX_CMDS["TARGET_SSH"][:]
        ffx_cmd.append(cmd)
        return self.run(ffx_cmd, timeout=timeout, capture_output=capture_output)

    def wait_for_rcs_connection(
        self,
        timeout: float = ffx_interface.TIMEOUTS["TARGET_RCS_CONNECTION_WAIT"],
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
        _LOGGER.info("Waiting for %s to connect to host...", self._target_name)

        cmd: list[str] = _FFX_CMDS["TARGET_WAIT"] + [str(timeout)]
        try:
            self.run(
                cmd=cmd,
                # check_output timeout should be > rcs_connection timeout passed
                timeout=timeout + 5,
            )
            _LOGGER.info("%s is connected to host", self._target_name)
            return
        except errors.DeviceNotConnectedError:
            raise
        except subprocess.TimeoutExpired as err:
            raise errors.FfxTimeoutError(
                f"`{cmd} command timed out after {timeout}sec."
            ) from err
        except Exception as err:  # pylint: disable=broad-except
            raise errors.FfxCommandError(
                f"'{self._target_name}' is still not connected to host even "
                f"after {timeout}sec."
            ) from err

    def wait_for_rcs_disconnection(
        self,
        timeout: float = ffx_interface.TIMEOUTS[
            "TARGET_RCS_DISCONNECTION_WAIT"
        ],
    ) -> None:
        """Wait until FFX is able to disconnect RCS connection to the target.

        Args:
            timeout: How long in seconds to wait for disconnection.

        Raises:
            errors.DeviceNotConnectedError: If FFX fails to reach target.
            errors.FfxTimeoutError: In case of FFX command timeout.
            errors.FfxCommandError: In case of other FFX command failure.
        """
        _LOGGER.info(
            "Waiting for %s to disconnect from host...", self._target_name
        )
        cmd: list[str] = _FFX_CMDS["TARGET_WAIT_DOWN"] + [str(timeout)]

        try:
            self.run(
                cmd=cmd,
                # check_output timeout should be > rcs_disconnect timeout passed
                timeout=timeout + 5,
            )
            _LOGGER.info("%s is not connected to host", self._target_name)
            return
        except errors.DeviceNotConnectedError:
            raise
        except subprocess.TimeoutExpired as err:
            raise errors.FfxTimeoutError(
                f"`{cmd} command timed out after {timeout}sec."
            ) from err
        except Exception as err:  # pylint: disable=broad-except
            raise errors.FfxCommandError(
                f"'{self._target_name}' is still connected to host even after"
                f" {timeout}sec."
            ) from err

    # List all private methods
    def _generate_ffx_cmd(
        self,
        cmd: list[str],
        include_target: bool = True,
    ) -> list[str]:
        """Generates the FFX command that need to be passed
        subprocess.check_output.

        Args:
            cmd: FFX command.
            include_target: True to include "-t <target_name>", False otherwise.

        Returns:
            FFX command to be run as list of string.
        """
        ffx_args: list[str] = []

        if include_target:
            ffx_args.extend(["-t", f"{self._target}"])

        # To run FFX in isolation mode
        ffx_args.extend(["--isolate-dir", self.config.isolate_dir.directory()])

        return [self.config.binary_path] + ffx_args + cmd
