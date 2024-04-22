#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.custom_types.py."""

import ipaddress
import unittest

from parameterized import parameterized

from honeydew.typing import custom_types


class CustomTypesTests(unittest.TestCase):
    """Unit tests for honeydew.custom_types.py."""

    @parameterized.expand(  # type: ignore[misc]
        [
            (
                "valid_ipv4",
                "127.0.0.1:8081",
                custom_types.IpPort(
                    ip=ipaddress.ip_address("127.0.0.1"), port=8081
                ),
            ),
            (
                "valid_ipv6",
                "[::1]:8081",
                custom_types.IpPort(ip=ipaddress.ip_address("::1"), port=8081),
            ),
            (
                "valid_ipv6_scope",
                "[::1%eth0]:8081",
                custom_types.IpPort(
                    ip=ipaddress.ip_address("::1%eth0"), port=8081
                ),
            ),
            (
                "valid_ipv6_scope_digit",
                "[::1%123]:8081",
                custom_types.IpPort(
                    ip=ipaddress.ip_address("::1%123"), port=8081
                ),
            ),
            (
                "valid_ipv6_no_brackets",
                "::1:8081",
                custom_types.IpPort(ip=ipaddress.ip_address("::1"), port=8081),
            ),
        ]
    )
    def test_create_using_ip_and_port(
        self, _: str, addr: str, expected: custom_types.IpPort
    ) -> None:
        """Test cases for IpPort.create_using_ip_and_port()."""
        got: custom_types.IpPort = custom_types.IpPort.create_using_ip_and_port(
            addr
        )
        self.assertEqual(got, expected)

    @parameterized.expand(  # type: ignore[misc]
        [
            (
                "valid_ipv4",
                "127.0.0.1",
                custom_types.IpPort(
                    ip=ipaddress.ip_address("127.0.0.1"), port=None
                ),
            ),
            (
                "valid_ipv6",
                "::1",
                custom_types.IpPort(ip=ipaddress.ip_address("::1"), port=None),
            ),
            (
                "valid_ipv6_scope",
                "::1%eth0",
                custom_types.IpPort(
                    ip=ipaddress.ip_address("::1%eth0"), port=None
                ),
            ),
            (
                "valid_ipv6_scope_digit",
                "::1%123",
                custom_types.IpPort(
                    ip=ipaddress.ip_address("::1%123"), port=None
                ),
            ),
            (
                "valid_ipv6_with_brackets",
                "[::1]",
                custom_types.IpPort(ip=ipaddress.ip_address("::1"), port=None),
            ),
        ]
    )
    def test_create_using_ip(
        self, _: str, addr: str, expected: custom_types.IpPort
    ) -> None:
        """Test cases for IpPort.create_using_ip()."""
        got: custom_types.IpPort = custom_types.IpPort.create_using_ip(addr)
        self.assertEqual(got, expected)

    @parameterized.expand(  # type: ignore[misc]
        [
            ("invalid", "some_str"),
            ("invalid_double_scope", "[::1%e%eth0]:100"),
            ("invalid_double_percent", "[::1%%eth0]:100"),
            ("invalid_negative_port", "[::1]:-1"),
            ("invalid_zero_port", "[::1]:0"),
            ("invalid_port_number", "[::1]:asdf"),
        ]
    )
    def test_create_using_ip_and_port_raises(self, _: str, addr: str) -> None:
        """Test cases for IpPort.create_using_ip_and_port() which raise
        exceptions."""
        with self.assertRaises(ValueError):
            custom_types.IpPort.create_using_ip_and_port(addr)

    @parameterized.expand(  # type: ignore[misc]
        [
            ("invalid", "some_str"),
            ("invalid_double_scope", "[::1%e%eth0]"),
            ("invalid_double_percent", "[::1%%eth0]"),
        ]
    )
    def test_create_using_ip_raises(self, _: str, addr: str) -> None:
        """Test cases for IpPort.create_using_ip() which raise exceptions."""
        with self.assertRaises(ValueError):
            custom_types.IpPort.create_using_ip(addr)

    @parameterized.expand(  # type: ignore[misc]
        [
            (
                "valid_ipv4_and_port",
                custom_types.IpPort(
                    ip=ipaddress.ip_address("127.0.0.1"), port=8081
                ),
                "127.0.0.1:8081",
            ),
            (
                "valid_ipv6_and_port",
                custom_types.IpPort(ip=ipaddress.ip_address("::1"), port=8081),
                "[::1]:8081",
            ),
            (
                "valid_ipv6_with_scope_and_port",
                custom_types.IpPort(
                    ip=ipaddress.ip_address("::1%eth0"), port=8081
                ),
                "[::1%eth0]:8081",
            ),
            (
                "valid_ipv4_without_port",
                custom_types.IpPort(
                    ip=ipaddress.ip_address("127.0.0.1"), port=None
                ),
                "127.0.0.1",
            ),
            (
                "valid_ipv6_without_port",
                custom_types.IpPort(
                    ip=ipaddress.ip_address("::1%eth0"), port=None
                ),
                "[::1%eth0]",
            ),
        ]
    )
    def test_ipport_str(
        self, _: str, ip_port: custom_types.IpPort, expected: str
    ) -> None:
        """Test cases for IpPort.__str__."""
        got = str(ip_port)
        self.assertEqual(got, expected)


if __name__ == "__main__":
    unittest.main()
