# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
Tests of various (sometimes hardcoded) properties of ifaces.
"""

import asyncio

from mobly import base_test, test_runner
from mobly.asserts import assert_equal
from wlanix_testing import base_test


class IfaceCorrectnessAndConsistencyTest(base_test.IfaceBaseTestClass):
    # TODO(https://fxbug.dev/368005870): Need to reconsider the consequences of using
    # the same iface name, even when an iface is recreated.
    def test_iface_name_hardcoded_as_wlan(self) -> None:
        response = asyncio.run(self.wifi_sta_iface_proxy.get_name()).unwrap()
        assert_equal(
            response.iface_name,
            "wlan",
            'WifiStaIface should always return the hardcoded iface name "wlan"',
        )

    def test_iface_name_consistency(self) -> None:
        response = asyncio.run(
            self.wifi_chip_proxy.get_sta_iface_names()
        ).unwrap()
        assert_equal(
            len(response.iface_names),
            1,
            "WifiChip should have returned the iface just created",
        )

        iface_name = response.iface_names[0]
        response = asyncio.run(self.wifi_sta_iface_proxy.get_name()).unwrap()
        assert_equal(
            iface_name,
            response.iface_name,
            "WifiStaIface returns a different name than WifiChip",
        )


if __name__ == "__main__":
    test_runner.main()
