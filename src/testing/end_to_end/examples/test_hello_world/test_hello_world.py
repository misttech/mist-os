# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import logging

from fuchsia_base_test import fuchsia_base_test
from mobly import test_runner

_LOGGER: logging.Logger = logging.getLogger(__name__)


class HelloWorldTest(fuchsia_base_test.FuchsiaBaseTest):
    def setup_class(self):
        """Initialize all DUT(s)"""
        super().setup_class()

    def test_hello_world(self):
        for fuchsia_device in self.fuchsia_devices:
            _LOGGER.info(f"{fuchsia_device.device_name} says hello!")


if __name__ == "__main__":
    test_runner.main()
