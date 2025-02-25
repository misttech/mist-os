# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Provides methods to configure FFX."""

import atexit
import logging
from dataclasses import dataclass

import fuchsia_controller_py as fuchsia_controller

from honeydew import errors
from honeydew.utils import host_shell

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


_LOGGER: logging.Logger = logging.getLogger(__name__)


class FfxConfigError(errors.HoneydewError):
    """Raised by FfxConfig class."""


@dataclass(frozen=True)
class FfxConfigData:
    """Dataclass that holds FFX config information.

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
        proxy_timeout_secs: Proxy timeout in secs.
        ssh_keepalive_timeout: SSH keep-alive timeout in secs.
    """

    binary_path: str
    isolate_dir: fuchsia_controller.IsolateDir
    logs_dir: str
    logs_level: str | None
    mdns_enabled: bool
    subtools_search_path: str | None
    proxy_timeout_secs: int | None
    ssh_keepalive_timeout: int | None

    def __str__(self) -> str:
        return (
            f"binary_path={self.binary_path}, "
            f"isolate_dir={self.isolate_dir.directory()}, "
            f"logs_dir={self.logs_dir}, "
            f"logs_level={self.logs_level}, "
            f"mdns_enabled={self.mdns_enabled}, "
            f"subtools_search_path={self.subtools_search_path}, "
            f"proxy_timeout_secs={self.proxy_timeout_secs}, "
            f"ssh_keepalive_timeout={self.ssh_keepalive_timeout}, "
        )


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
            FfxConfigError: If setup has already been called once.

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
            raise FfxConfigError("setup has already been called once.")

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
            FfxConfigError: When called before calling `FfxConfig.setup`
        """
        if self._setup_done is False:
            raise FfxConfigError("close called before calling setup.")

        # Setting to None will delete the `self._isolate_dir.directory()`
        self._isolate_dir = None

        self._setup_done = False

    def get_config(self) -> FfxConfigData:
        """Returns the FFX configuration information that has been set.

        Returns:
            FfxConfigData

        Raises:
            FfxConfigError: When called before `FfxConfig.setup` or after `FfxConfig.close`.
        """
        if self._setup_done is False:
            raise FfxConfigError("get_config called before calling setup.")
        if self._isolate_dir is None:
            raise FfxConfigError("get_config called after calling close.")

        return FfxConfigData(
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
        except FfxConfigError:
            pass

    def _run(
        self,
        cmd: list[str],
    ) -> None:
        """Executes `ffx {cmd}`.

        Args:
            cmd: FFX command to run.

        Raises:
            FfxConfigError: In case of any other FFX command failure, or
                when called after `FfxConfig.close`.
        """
        if self._isolate_dir is None:
            raise FfxConfigError("_run called after calling close.")

        ffx_args: list[str] = []
        ffx_args.extend(["--isolate-dir", self._isolate_dir.directory()])
        ffx_cmd: list[str] = [self._ffx_binary] + ffx_args + cmd

        try:
            host_shell.run(cmd=ffx_cmd)
            return
        except errors.HostCmdError as err:
            raise FfxConfigError(err) from err
