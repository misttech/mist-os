# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for ffx.config.py."""

import unittest
from typing import Any
from unittest import mock

import fuchsia_controller_py as fuchsia_controller

from honeydew import errors
from honeydew.transports.ffx import config as ffx_config
from honeydew.utils import host_shell

# pylint: disable=protected-access
_TARGET_NAME: str = "fuchsia-emulator"

_ISOLATE_DIR: str = "/tmp/isolate"
_LOGS_DIR: str = "/tmp/logs"
_BINARY_PATH: str = "ffx"
_LOGS_LEVEL: str = "debug"
_MDNS_ENABLED: bool = False
_SUBTOOLS_SEARCH_PATH: str = "/subtools"
_PROXY_TIMEOUT_SECS: int = 30
_SSH_KEEPALIVE_TIMEOUT: int = 60

_FFX_CMD_OPTIONS: list[str] = [
    "ffx",
    "--isolate-dir",
    _ISOLATE_DIR,
]

_FFX_CONFIG_SET: list[str] = _FFX_CMD_OPTIONS + [
    "config",
    "set",
]

_INPUT_ARGS: dict[str, Any] = {
    "target_name": _TARGET_NAME,
    "ffx_config_data": ffx_config.FfxConfigData(
        isolate_dir=fuchsia_controller.IsolateDir(_ISOLATE_DIR),
        logs_dir=_LOGS_DIR,
        binary_path=_BINARY_PATH,
        logs_level=_LOGS_LEVEL,
        mdns_enabled=_MDNS_ENABLED,
        subtools_search_path=_SUBTOOLS_SEARCH_PATH,
        proxy_timeout_secs=_PROXY_TIMEOUT_SECS,
        ssh_keepalive_timeout=_SSH_KEEPALIVE_TIMEOUT,
    ),
}


class FfxConfigTests(unittest.TestCase):
    """Unit tests for ffx.config.FfxConfig"""

    @mock.patch.object(
        host_shell,
        "run",
        autospec=True,
    )
    def test_setup(self, mock_host_shell_run: mock.Mock) -> None:
        """Test case for FfxConfig.setup()"""

        ffx_config_obj: ffx_config.FfxConfig = ffx_config.FfxConfig()

        ffx_config_obj.setup(
            binary_path=_BINARY_PATH,
            isolate_dir=_ISOLATE_DIR,
            logs_dir=_LOGS_DIR,
            logs_level=_LOGS_LEVEL,
            enable_mdns=_MDNS_ENABLED,
            subtools_search_path=_SUBTOOLS_SEARCH_PATH,
            proxy_timeout_secs=_PROXY_TIMEOUT_SECS,
            ssh_keepalive_timeout=_SSH_KEEPALIVE_TIMEOUT,
        )

        ffx_configs_calls = [
            mock.call(_FFX_CONFIG_SET + ["log.dir", _LOGS_DIR]),
            mock.call(_FFX_CONFIG_SET + ["log.level", _LOGS_LEVEL.lower()]),
            mock.call(
                _FFX_CONFIG_SET
                + ["discovery.mdns.enabled", str(_MDNS_ENABLED).lower()]
            ),
            mock.call(
                _FFX_CONFIG_SET
                + ["proxy.timeout_secs", str(_PROXY_TIMEOUT_SECS)]
            ),
            mock.call(
                _FFX_CONFIG_SET
                + ["ssh.keepalive_timeout", str(_SSH_KEEPALIVE_TIMEOUT)]
            ),
            mock.call(
                _FFX_CONFIG_SET
                + ["ffx.subtool-search-paths", _SUBTOOLS_SEARCH_PATH]
            ),
            mock.call(_FFX_CMD_OPTIONS + ["daemon", "start", "--background"]),
        ]
        mock_host_shell_run.assert_has_calls(ffx_configs_calls, any_order=True)

        # Calling setup() again should fail
        with self.assertRaises(ffx_config.FfxConfigError):
            ffx_config_obj.setup(
                binary_path=_BINARY_PATH,
                isolate_dir=_ISOLATE_DIR,
                logs_dir=_LOGS_DIR,
                logs_level=_LOGS_LEVEL,
                enable_mdns=_MDNS_ENABLED,
                subtools_search_path=_SUBTOOLS_SEARCH_PATH,
                proxy_timeout_secs=_PROXY_TIMEOUT_SECS,
                ssh_keepalive_timeout=_SSH_KEEPALIVE_TIMEOUT,
            )

    @mock.patch.object(
        host_shell,
        "run",
        side_effect=errors.HostCmdError(
            "error",
        ),
        autospec=True,
    )
    def test_setup_raises_ffx_config_error(
        self, mock_host_shell_run: mock.Mock
    ) -> None:
        """Test case for ffx_config.FfxConfig.setup() raises FfxConfigError"""

        ffx_config_obj: ffx_config.FfxConfig = ffx_config.FfxConfig()

        with self.assertRaises(ffx_config.FfxConfigError):
            ffx_config_obj.setup(
                binary_path=_BINARY_PATH,
                isolate_dir=_ISOLATE_DIR,
                logs_dir=_LOGS_DIR,
                logs_level=_LOGS_LEVEL,
                enable_mdns=_MDNS_ENABLED,
                subtools_search_path=_SUBTOOLS_SEARCH_PATH,
                proxy_timeout_secs=_PROXY_TIMEOUT_SECS,
                ssh_keepalive_timeout=_SSH_KEEPALIVE_TIMEOUT,
            )

        mock_host_shell_run.assert_called()

    @mock.patch.object(
        ffx_config.FfxConfig,
        "_run",
        autospec=True,
    )
    def test_close(self, mock_ffx_config_run: mock.Mock) -> None:
        """Test case for ffx_config.FfxConfig.close()"""

        ffx_config_obj: ffx_config.FfxConfig = ffx_config.FfxConfig()

        # Call setup first before calling close
        ffx_config_obj.setup(
            binary_path=_BINARY_PATH,
            isolate_dir=_ISOLATE_DIR,
            logs_dir=_LOGS_DIR,
            logs_level=_LOGS_LEVEL,
            enable_mdns=_MDNS_ENABLED,
            subtools_search_path=_SUBTOOLS_SEARCH_PATH,
            proxy_timeout_secs=_PROXY_TIMEOUT_SECS,
            ssh_keepalive_timeout=_SSH_KEEPALIVE_TIMEOUT,
        )
        mock_ffx_config_run.assert_called()

        ffx_config_obj.close()

    def test_close_without_setup(self) -> None:
        """Test case for ffx_config.FfxConfig.close() without calling
        ffx_config.FfxConfig.setup()"""

        ffx_config_obj: ffx_config.FfxConfig = ffx_config.FfxConfig()

        # Calling setup() again should fail
        with self.assertRaises(ffx_config.FfxConfigError):
            ffx_config_obj.close()

    @mock.patch.object(
        ffx_config.FfxConfig,
        "_run",
        autospec=True,
    )
    def test_get_config(self, mock_ffx_config_run: mock.Mock) -> None:
        """Test case for ffx_config.FfxConfig.get_config()"""

        ffx_config_obj: ffx_config.FfxConfig = ffx_config.FfxConfig()

        # Call setup first before calling close
        ffx_config_obj.setup(
            binary_path=_BINARY_PATH,
            isolate_dir=_ISOLATE_DIR,
            logs_dir=_LOGS_DIR,
            logs_level=_LOGS_LEVEL,
            enable_mdns=_MDNS_ENABLED,
            subtools_search_path=_SUBTOOLS_SEARCH_PATH,
            proxy_timeout_secs=_PROXY_TIMEOUT_SECS,
            ssh_keepalive_timeout=_SSH_KEEPALIVE_TIMEOUT,
        )
        mock_ffx_config_run.assert_called()

        self.assertEqual(
            str(ffx_config_obj.get_config()),
            str(_INPUT_ARGS["ffx_config_data"]),
        )

    def test_get_config_without_setup(self) -> None:
        """Test case for ffx_config.FfxConfig.get_config() without calling
        ffx_config.FfxConfig.setup()"""

        ffx_config_obj: ffx_config.FfxConfig = ffx_config.FfxConfig()

        # Calling setup() again should fail
        with self.assertRaises(ffx_config.FfxConfigError):
            ffx_config_obj.get_config()
