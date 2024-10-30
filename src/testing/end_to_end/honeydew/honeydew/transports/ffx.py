# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Provides methods for Host-(Fuchsia)Target interactions via FFX."""

import atexit
import ipaddress
import json
import logging
import subprocess
from typing import Any

import fuchsia_controller_py as fuchsia_controller

from honeydew import errors
from honeydew.interfaces.transports import ffx as ffx_interface
from honeydew.typing import custom_types
from honeydew.typing.ffx import TargetInfoData
from honeydew.utils import host_shell, properties

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
    "PROXY_TIMEOUT": [
        "config",
        "set",
        "proxy.timeout_secs",
    ],
    "SSH_KEEPALIVE_TIMEOUT": [
        "config",
        "set",
        "ssh.keepalive_timeout",
    ],
    "DAEMON_START": [
        "daemon",
        "start",
        "--background",
    ],
}

_FFX_CMDS: dict[str, list[str]] = {
    "TARGET_ADD": ["target", "add"],
    "TARGET_SHOW": ["--machine", "json", "target", "show"],
    "TARGET_SSH_ADDRESS": ["target", "get-ssh-address"],
    "TARGET_LIST": ["--machine", "json", "target", "list"],
    "TARGET_WAIT": ["target", "wait", "--timeout", "0"],
    "TARGET_WAIT_DOWN": ["target", "wait", "--down", "--timeout", "0"],
    # Tell the daemon to drop its connection to the target
    "TARGET_DISCONNECT": ["daemon", "disconnect"],
    "TEST_RUN": ["test", "run"],
    "TARGET_SSH": ["target", "ssh"],
}

_LOGGER: logging.Logger = logging.getLogger(__name__)

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
        subtools_search_path: str | None = None,
        proxy_timeout_secs: int | None = None,
        ssh_keepalive_timeout: int | None = None,
    ) -> None:
        """Sets up configuration need to be used while running FFX command.

        Args:
            binary_path: absolute path to the FFX binary.
            isolate_dir: Directory that will be passed to `--isolate-dir`
                arg of FFX. If set to None, a random directory will be created.
            logs_dir: Directory that will be passed to `--config log.dir`
                arg of FFX
            logs_level: logs level that will be passed to `--config log.level`
                arg of FFX
            enable_mdns: Whether or not mdns need to be enabled. This will be
                passed to `--config discovery.mdns.enabled` arg of FFX
            subtools_search_path: A path of where ffx should look for plugins.
                Default value is None which means, it will not update
                proxy_timeout_secs
            proxy_timeout_secs: Proxy timeout in secs. Default value is None
                which means, it will not update proxy_timeout_secs
            ssh_keepalive_timeout: SSH keep-alive timeout in secs.
                Default value is None which means, it will not update
                ssh_keepalive_timeout

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
        self._isolate_dir: fuchsia_controller.IsolateDir | None = (
            fuchsia_controller.IsolateDir(isolate_dir)
        )
        self._logs_dir: str = logs_dir
        self._logs_level: str | None = logs_level
        self._mdns_enabled: bool = enable_mdns
        self._subtools_search_path: str | None = subtools_search_path
        self._proxy_timeout_secs: int | None = proxy_timeout_secs
        self._ssh_keepalive_timeout: int | None = ssh_keepalive_timeout

        self._run(_FFX_CONFIG_CMDS["LOG_DIR"] + [self._logs_dir])

        if self._logs_level is not None:
            self._run(_FFX_CONFIG_CMDS["LOG_LEVEL"] + [self._logs_level])

        self._run(_FFX_CONFIG_CMDS["MDNS"] + [str(self._mdns_enabled).lower()])

        # Setting this based on the recommendation from awdavies@ for below
        # FuchsiaController error:
        #   FFX Library Error: Timeout attempting to reach target
        if self._proxy_timeout_secs is not None:
            self._run(
                _FFX_CONFIG_CMDS["PROXY_TIMEOUT"]
                + [str(self._proxy_timeout_secs)]
            )

        if self._ssh_keepalive_timeout is not None:
            self._run(
                _FFX_CONFIG_CMDS["SSH_KEEPALIVE_TIMEOUT"]
                + [str(self._ssh_keepalive_timeout)]
            )

        if self._subtools_search_path is not None:
            self._run(
                _FFX_CONFIG_CMDS["SUB_TOOLS_PATH"]
                + [self._subtools_search_path]
            )

        self._run(_FFX_CONFIG_CMDS["DAEMON_START"])

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
            errors.FfxConfigError: When called before `FfxConfig.setup` or after
                `FfxConfig.close`.
        """
        if self._setup_done is False:
            raise errors.FfxConfigError(
                "get_config called before calling setup."
            )
        if self._isolate_dir is None:
            raise errors.FfxConfigError(
                "get_config called after calling close."
            )

        return custom_types.FFXConfig(
            binary_path=self._ffx_binary,
            isolate_dir=self._isolate_dir,
            logs_dir=self._logs_dir,
            logs_level=self._logs_level,
            mdns_enabled=self._mdns_enabled,
            subtools_search_path=self._subtools_search_path,
            proxy_timeout_secs=self._proxy_timeout_secs,
            ssh_keepalive_timeout=self._ssh_keepalive_timeout,
        )

    def _atexit_callback(self) -> None:
        try:
            self.close()
        except errors.FfxConfigError:
            pass

    def _run(
        self,
        cmd: list[str],
    ) -> None:
        """Executes `ffx {cmd}`.

        Args:
            cmd: FFX command to run.

        Raises:
            errors.FfxConfigError: In case of any other FFX command failure, or
                when called after `FfxConfig.close`.
        """
        if self._isolate_dir is None:
            raise errors.FfxConfigError("_run called after calling close.")

        ffx_args: list[str] = []
        ffx_args.extend(["--isolate-dir", self._isolate_dir.directory()])
        ffx_cmd: list[str] = [self._ffx_binary] + ffx_args + cmd

        try:
            host_shell.run(cmd=ffx_cmd)
            return
        except errors.HostCmdError as err:
            raise errors.FfxConfigError(err) from err


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

    def add_target(self) -> None:
        """Adds a target to the ffx collection

        Raises:
            errors.DeviceNotConnectedError: If FFX fails to reach target.
            errors.FfxCommandError: In case of other FFX command failure.
        """
        cmd: list[str] = _FFX_CMDS["TARGET_ADD"] + [str(self._target_ip_port)]
        ffx_cmd: list[str] = self.generate_ffx_cmd(
            cmd=cmd, include_target=False
        )

        try:
            host_shell.run(cmd=ffx_cmd)
        except errors.HostCmdError as err:
            if _DEVICE_NOT_CONNECTED in str(err):
                raise errors.DeviceNotConnectedError(
                    f"{self._target_name} is not connected to host"
                ) from err
            raise errors.FfxCommandError(err) from err

    def check_connection(self) -> None:
        """Checks the FFX connection from host to Fuchsia device.

        Raises:
            errors.FfxConnectionError
        """
        try:
            self.wait_for_rcs_connection()
        except errors.HoneydewError as err:
            raise errors.FfxConnectionError(
                f"FFX connection check failed for {self._target_name} with err: {err}"
            ) from err

    def get_target_information(self) -> TargetInfoData:
        """Executed and returns the output of `ffx -t {target} target show`.

        Returns:
            Output of `ffx -t {target} target show`.

        Raises:
            errors.DeviceNotConnectedError: If FFX fails to reach target.
            errors.FfxCommandError: In case of other FFX command failure.
        """
        cmd: list[str] = _FFX_CMDS["TARGET_SHOW"]
        output: str = self.run(cmd=cmd)

        target_info = TargetInfoData(**json.loads(output))
        _LOGGER.debug("`%s` returned: %s", " ".join(cmd), target_info)

        return target_info

    def get_target_info_from_target_list(self) -> dict[str, Any]:
        """Executed and returns the output of
        `ffx --machine json target list <target>`.

        Returns:
            Output of `ffx --machine json target list <target>`.

        Raises:
            errors.FfxCommandError: In case of FFX command failure.
        """
        cmd: list[str] = _FFX_CMDS["TARGET_LIST"] + [self._target]
        output: str = self.run(
            cmd=cmd,
            include_target=False,
        )

        target_info_from_target_list: list[dict[str, Any]] = json.loads(output)
        _LOGGER.debug(
            "`%s` returned: %s", " ".join(cmd), target_info_from_target_list
        )

        if len(target_info_from_target_list) == 1:
            return target_info_from_target_list[0]
        else:
            raise errors.FfxCommandError(
                f"'{self._target_name}' is not connected to host"
            )

    def get_target_name(self) -> str:
        """Returns the target name.

        Returns:
            Target name.

        Raises:
            errors.DeviceNotConnectedError: If FFX fails to reach target.
            errors.FfxCommandError: In case of other FFX command failure.
        """
        ffx_target_show_info: TargetInfoData = self.get_target_information()
        return ffx_target_show_info.target.name

    def get_target_ssh_address(self) -> custom_types.TargetSshAddress:
        """Returns the target's ssh ip address and port information.

        Returns:
            (Target SSH IP Address, Target SSH Port)

        Raises:
            errors.DeviceNotConnectedError: If FFX fails to reach target.
            errors.FfxCommandError: In case of other FFX command failure.
        """
        cmd: list[str] = _FFX_CMDS["TARGET_SSH_ADDRESS"]
        output: str = self.run(cmd=cmd)

        # in '[fe80::6a47:a931:1e84:5077%qemu]:22', ":22" is SSH port.
        # Ports can be 1-5 chars, clip off everything after the last ':'.
        ssh_info: list[str] = output.rsplit(":", 1)
        ssh_ip: str = ssh_info[0].replace("[", "").replace("]", "")
        ssh_port: int = int(ssh_info[1])

        return custom_types.TargetSshAddress(
            ip=ipaddress.ip_address(ssh_ip), port=ssh_port
        )

    def get_target_board(self) -> str:
        """Returns the target's board.

        Returns:
            Target's board.

        Raises:
            errors.DeviceNotConnectedError: If FFX fails to reach target.
            errors.FfxCommandError: In case of other FFX command failure.
        """
        target_show_info: TargetInfoData = self.get_target_information()
        return (
            target_show_info.build.board if target_show_info.build.board else ""
        )

    def get_target_product(self) -> str:
        """Returns the target's product.

        Returns:
            Target's product.

        Raises:
            errors.DeviceNotConnectedError: If FFX fails to reach target.
            errors.FfxCommandError: In case of other FFX command failure.
        """
        target_show_info: TargetInfoData = self.get_target_information()
        return (
            target_show_info.build.product
            if target_show_info.build.product
            else ""
        )

    def run(
        self,
        cmd: list[str],
        timeout: float | None = None,
        capture_output: bool = True,
        log_output: bool = True,
        include_target: bool = True,
    ) -> str:
        """Runs an FFX command.

        Args:
            cmd: FFX command to run.
            timeout: Timeout to wait for the ffx command to return. By default,
                timeout is not set.
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
        ffx_cmd: list[str] = self.generate_ffx_cmd(
            cmd=cmd,
            include_target=include_target,
        )
        try:
            return (
                host_shell.run(
                    cmd=ffx_cmd,
                    capture_output=capture_output,
                    log_output=log_output,
                    timeout=timeout,
                )
                or ""
            )
        except errors.HostCmdError as err:
            if _DEVICE_NOT_CONNECTED in str(err):
                raise errors.DeviceNotConnectedError(
                    f"{self._target_name} is not connected to host"
                ) from err
            raise errors.FfxCommandError(err) from err
        except errors.HoneydewTimeoutError as err:
            raise errors.FfxTimeoutError(err) from err

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
        return host_shell.popen(
            cmd=self.generate_ffx_cmd(cmd=cmd),
            **kwargs,
        )

    def run_test_component(
        self,
        component_url: str,
        ffx_test_args: list[str] | None = None,
        test_component_args: list[str] | None = None,
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
            capture_output: When True, the stdout/err from the command will be captured and
                returned. When False, the output of the command will be streamed to stdout/err
                accordingly and it won't be returned. Defaults to True.

        Returns:
            Output of `ffx -t {target} {cmd}` when capture_output is set to True, otherwise an
            empty string.

        Raises:
            errors.DeviceNotConnectedError: If FFX fails to reach target.
            errors.FfxCommandError: In case of other FFX command failure.
        """
        cmd: list[str] = _FFX_CMDS["TEST_RUN"][:]
        cmd.append(component_url)
        if ffx_test_args:
            cmd += ffx_test_args
        if test_component_args:
            cmd.append("--")
            cmd += test_component_args
        return self.run(cmd, capture_output=capture_output)

    def run_ssh_cmd(
        self,
        cmd: str,
        capture_output: bool = True,
    ) -> str:
        """Executes and returns the output of `ffx -t target ssh <cmd>`.

        Args:
            cmd: SSH command to run.
            capture_output: When True, the stdout/err from the command will be
                captured and returned. When False, the output of the command
                will be streamed to stdout/err accordingly and it won't be
                returned. Defaults to True.

        Returns:
            Output of `ffx -t target ssh <cmd>` when capture_output is set to
            True, otherwise an empty string.

        Raises:
            errors.DeviceNotConnectedError: If FFX fails to reach target.
            errors.FfxCommandError: In case of other FFX command failure.
        """
        ffx_cmd: list[str] = _FFX_CMDS["TARGET_SSH"][:]
        ffx_cmd.append(cmd)
        return self.run(ffx_cmd, capture_output=capture_output)

    def wait_for_rcs_connection(self) -> None:
        """Wait until FFX is able to establish a RCS connection to the target.

        Raises:
            errors.DeviceNotConnectedError: If FFX fails to reach target.
            errors.FfxCommandError: In case of other FFX command failure.
        """
        _LOGGER.info("Waiting for %s to connect to host...", self._target_name)

        self.run(cmd=_FFX_CMDS["TARGET_WAIT"])

        _LOGGER.info("%s is connected to host", self._target_name)
        return

    def wait_for_rcs_disconnection(self) -> None:
        """Wait until FFX is able to disconnect RCS connection to the target.

        Raises:
            errors.DeviceNotConnectedError: If FFX fails to reach target.
            errors.FfxCommandError: In case of other FFX command failure.
        """
        _LOGGER.info(
            "Waiting for %s to disconnect from host...", self._target_name
        )
        self.run(cmd=_FFX_CMDS["TARGET_WAIT_DOWN"])
        _LOGGER.info("%s is not connected to host", self._target_name)

        _LOGGER.debug(
            "Informing the FFX daemon to drop the connection to %s",
            self._target_name,
        )
        self.run(cmd=_FFX_CMDS["TARGET_DISCONNECT"])

        return

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
        ffx_args: list[str] = []

        if include_target:
            ffx_args.extend(["-t", f"{self._target}"])

        # To run FFX in isolation mode
        ffx_args.extend(["--isolate-dir", self.config.isolate_dir.directory()])

        return [self.config.binary_path] + ffx_args + cmd
