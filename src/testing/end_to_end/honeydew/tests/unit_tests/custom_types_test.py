#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.custom_types.py."""

import ipaddress
import unittest

from parameterized import parameterized

from honeydew import custom_types


class CustomTypesTests(unittest.TestCase):
    """Unit tests for honeydew.custom_types.py."""

    @parameterized.expand(
        [
            (
                "valid_ipv4", "127.0.0.1:8081",
                custom_types.IpPort(
                    ip=ipaddress.ip_address("127.0.0.1"), port=8081)),
            (
                "valid_ipv6", "[::1]:8081",
                custom_types.IpPort(ip=ipaddress.ip_address("::1"), port=8081)),
            (
                "valid_ipv6_scope", "[::1%eth0]:8081",
                custom_types.IpPort(
                    ip=ipaddress.ip_address("::1%eth0"), port=8081)),
            (
                "valid_ipv6_scope_digit", "[::1%123]:8081",
                custom_types.IpPort(
                    ip=ipaddress.ip_address("::1%123"), port=8081)),
            (
                "valid_ipv6_no_brackets", "::1:8081",
                custom_types.IpPort(ip=ipaddress.ip_address("::1"), port=8081)),
        ])
    def test_parse(self, _, addr: str, expected: custom_types.IpPort) -> None:
        """Test cases for IpPort.parse()."""
        got: custom_types.IpPort = custom_types.IpPort.parse(addr)
        self.assertEqual(got, expected)

    @parameterized.expand(
        [
            ("invalid", "some_str"),
            ("invalid_double_scope", "[::1%e%eth0]:100"),
            ("invalid_double_percent", "[::1%%eth0]:100"),
            ("invalid_negative_port", "[::1]:-1"),
            ("invalid_port_number", "[::1]:asdf"),
        ])
    def test_parse_raises(self, _, addr: str) -> None:
        """Test cases for IpPort.parse() which raise exceptions."""
        with self.assertRaises(ValueError):
            custom_types.IpPort.parse(addr)

    @parameterized.expand(
        [
            (
                "valid_ipv4",
                custom_types.IpPort(
                    ip=ipaddress.ip_address("127.0.0.1"),
                    port=8081), "127.0.0.1:8081"),
            (
                "valid_ipv6",
                custom_types.IpPort(ip=ipaddress.ip_address("::1"),
                                    port=8081), "[::1]:8081"),
            (
                "valid_ipv6_scope",
                custom_types.IpPort(
                    ip=ipaddress.ip_address("::1%eth0"),
                    port=8081), "[::1%eth0]:8081"),
        ])
    def test_ipport_str(
            self, _, ip_port: custom_types.IpPort, expected: str) -> None:
        """Test cases for IpPort.__str__."""
        got = str(ip_port)
        self.assertEqual(got, expected)


if __name__ == "__main__":
    unittest.main()
