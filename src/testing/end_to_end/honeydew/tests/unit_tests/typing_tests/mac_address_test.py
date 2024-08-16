# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.typing.wlan.MacAddress."""

import unittest

from honeydew.typing.wlan import MacAddress

_VALID_MAC_ADDRESS = "01:23:45:67:89:ab"
_VALID_MAC_ADDRESS_BYTES = bytes([1, 35, 69, 103, 137, 171])


class MacAddressTests(unittest.TestCase):
    """Unit tests for honeydew.typing.wlan.MacAddress."""

    def test_str(self) -> None:
        """Test if __str__ works."""
        mac = MacAddress(_VALID_MAC_ADDRESS)
        self.assertEqual(
            str(mac),
            _VALID_MAC_ADDRESS,
        )

    def test_bytes_valid(self) -> None:
        """Test if bytes works for valid MAC addresses."""
        mac = MacAddress(_VALID_MAC_ADDRESS)
        self.assertEqual(
            mac.bytes(),
            _VALID_MAC_ADDRESS_BYTES,
        )

    def test_bytes_invalid(self) -> None:
        """Test if bytes works for invalid MAC addresses."""
        for msg, mac in [
            ("not defined", ""),
            ("too short", "01:23:45:67:89"),
            ("invalid byte", "01:23:45:67:89:abcd"),
            ("too long", "01:23:45:67:89:ab:cd"),
            ("invalid hex", "hello world!"),
        ]:
            with self.subTest(msg=msg, mac=mac):
                with self.assertRaises(ValueError):
                    MacAddress(mac).bytes()


if __name__ == "__main__":
    unittest.main()
