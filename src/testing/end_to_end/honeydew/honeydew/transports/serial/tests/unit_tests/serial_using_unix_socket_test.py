# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.transports.serial_using_unix_socket.py."""

import socket
import unittest
from unittest import mock

from honeydew.transports.serial import errors as serial_errors
from honeydew.transports.serial import serial_using_unix_socket


class FastbootTests(unittest.TestCase):
    """Unit tests for honeydew.transports.serial_using_unix_socket.py."""

    def setUp(self) -> None:
        super().setUp()

        self.serial_obj: serial_using_unix_socket.SerialUsingUnixSocket = (
            serial_using_unix_socket.SerialUsingUnixSocket(
                device_name="device_name",
                socket_path="socket_path",
            )
        )

    @mock.patch.object(
        socket,
        "socket",
        autospec=True,
    )
    def test_send(self, mock_socket: mock.Mock) -> None:
        """Test case for serial_using_unix_socket.Socket.send()"""
        self.serial_obj.send(cmd="echo hello")
        mock_socket.assert_called()

    @mock.patch.object(
        socket, "socket", autospec=True, side_effect=socket.error
    )
    def test_send_error(self, mock_socket: mock.Mock) -> None:
        """Test case for serial_using_unix_socket.Socket.send() raising exception"""
        with self.assertRaises(serial_errors.SerialError):
            self.serial_obj.send(cmd="echo hello")
        mock_socket.assert_called()
