#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.fuchsia_controller.wlan.py."""

import unittest

from honeydew.affordances.fuchsia_controller.wlan import wlan as fc_wlan


# pylint: disable=protected-access
class WlanFCTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.fuchsia_controller.wlan.py."""

    def setUp(self) -> None:
        super().setUp()

        self.wlan_obj = fc_wlan.Wlan()

    def test_get_phy_ids(self) -> None:
        """Test for Wlan.get_phy_ids()."""
        with self.assertRaises(NotImplementedError):
            self.wlan_obj.get_phy_ids()


if __name__ == "__main__":
    unittest.main()
