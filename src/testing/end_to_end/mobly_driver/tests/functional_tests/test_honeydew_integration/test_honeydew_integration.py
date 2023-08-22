#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly Driver honeydew integration test.

Test that Mobly driver's Mobly config interfaces with HoneyDew correctly.
"""

from mobly import asserts
from mobly import base_test
from mobly import test_runner

from mobly_controller import fuchsia_device


class MoblyDriverHoneydewIntegrationTest(base_test.BaseTestClass):
    """Mobly Driver Honeydew integration tests."""

    def test_mobly_controller_init(self):
        """Test case to ensure Mobly controller initializes successfully"""
        fuchsia_devices = self.register_controller(fuchsia_device)
        asserts.assert_true(
            fuchsia_devices, 'Expect at least 1 created controller.')


if __name__ == '__main__':
    test_runner.main()
