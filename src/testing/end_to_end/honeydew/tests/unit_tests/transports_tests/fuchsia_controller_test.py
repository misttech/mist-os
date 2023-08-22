#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.transports.fuchsia_controller.py."""

from typing import Any, Dict
import unittest
from unittest import mock

import fuchsia_controller_py as fuchsia_controller

from honeydew import custom_types
from honeydew import errors
from honeydew.transports import \
    fuchsia_controller as fuchsia_controller_transport

_INPUT_ARGS: Dict[str, Any] = {
    "device_name": "fuchsia-emulator",
}

_MOCK_ARGS: Dict[str, Any] = {
    "ffx_config":
        custom_types.FFXConfig(
            isolate_dir=fuchsia_controller.IsolateDir("/tmp/isolate"),
            logs_dir="/tmp/logs"),
}


# pylint: disable=protected-access
class FuchsiaControllerTests(unittest.TestCase):
    """Unit tests for honeydew.transports.fuchsia_controller.py."""

    def setUp(self) -> None:
        super().setUp()

        self.fuchsia_controller_obj = \
                fuchsia_controller_transport.FuchsiaController(
                    device_name=_INPUT_ARGS["device_name"])

    @mock.patch.object(
        fuchsia_controller_transport.fd_remotecontrol.RemoteControl,
        "Client",
        autospec=True)
    @mock.patch.object(
        fuchsia_controller_transport.fuchsia_controller,
        "Context",
        autospec=True)
    @mock.patch.object(
        fuchsia_controller_transport.ffx_transport,
        "get_config",
        return_value=_MOCK_ARGS["ffx_config"],
        autospec=True)
    def test_create_context(
            self, mock_ffx_get_config, mock_fc_context,
            mock_remote_control_proxy) -> None:
        """Test case for fuchsia_controller_transport.create_context()."""
        self.fuchsia_controller_obj.create_context()

        mock_ffx_get_config.assert_called_once()
        mock_fc_context.assert_called_once_with(
            config=mock.ANY,
            isolate_dir=_MOCK_ARGS["ffx_config"].isolate_dir,
            target=self.fuchsia_controller_obj._name)
        mock_remote_control_proxy.assert_called()

    @mock.patch.object(
        fuchsia_controller_transport.fuchsia_controller,
        "Context",
        autospec=True)
    def test_create_context_creation_error(self, mock_fc_context) -> None:
        """Verify create_context() when the fuchsia controller Context creation
        raises an error."""
        mock_fc_context.side_effect = fuchsia_controller.ZxStatus(
            fuchsia_controller.ZxStatus.ZX_ERR_INVALID_ARGS)

        with self.assertRaises(errors.FuchsiaControllerError):
            self.fuchsia_controller_obj.create_context()

        mock_fc_context.assert_called()

    @mock.patch.object(
        fuchsia_controller_transport.fd_remotecontrol.RemoteControl,
        "Client",
        autospec=True)
    @mock.patch.object(
        fuchsia_controller_transport.fuchsia_controller,
        "Context",
        autospec=True)
    def test_create_context_proxy_error(
            self, mock_fc_context, mock_remote_control_proxy) -> None:
        """Verify create_context() when the RemoteControl proxy creation raises
        an error."""
        mock_remote_control_proxy.side_effect = fuchsia_controller.ZxStatus(
            fuchsia_controller.ZxStatus.ZX_ERR_INVALID_ARGS)

        with self.assertRaises(errors.FuchsiaControllerError):
            self.fuchsia_controller_obj.create_context()

        mock_fc_context.assert_called()
        mock_remote_control_proxy.assert_called()

    def test_destroy_context(self) -> None:
        """Test case for fuchsia_controller_transport.destroy_context()"""
        self.fuchsia_controller_obj.destroy_context()

    @mock.patch.object(
        fuchsia_controller_transport.fd_remotecontrol.RemoteControl,
        "Client",
        autospec=True)
    @mock.patch.object(
        fuchsia_controller_transport.fuchsia_controller,
        "Context",
        autospec=True)
    def test_connect_device_proxy(
            self, mock_fc_context, mock_remote_control_proxy) -> None:
        """Test case for fuchsia_controller_transport.connect_device_proxy()"""
        self.fuchsia_controller_obj.create_context()

        self.fuchsia_controller_obj.connect_device_proxy(
            fuchsia_controller_transport._FC_PROXIES["RemoteControl"])

        mock_fc_context.assert_called()
        mock_remote_control_proxy.assert_called()
