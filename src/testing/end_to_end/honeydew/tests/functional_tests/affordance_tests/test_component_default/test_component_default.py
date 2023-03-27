#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for affordances/component_default.py."""

import logging

from honeydew.interfaces.device_classes import component_capable_device
from honeydew.mobly_controller import fuchsia_device
from mobly import asserts, base_test, test_runner

ROOT_CM_URL = "fuchsia-boot:///#meta/root.cm"
SYS_MGR_V1_CMX_NAME = "sysmgr.cmx"
ROOT_V2_CM_NAME = "root.cm"
UNKNOWN_COMPONENT_NAME = "unknown.cm"

_LOGGER = logging.getLogger(__name__)


class ComponentCapabilityTests(base_test.BaseTestClass):
    """Component capability tests"""

    def setup_class(self):
        """setup_class is called once before running tests."""
        fuchsia_devices = self.register_controller(fuchsia_device)
        self.device = fuchsia_devices[0]

    def on_fail(self, _):
        """on_fail is called once when a test case fails."""
        if not hasattr(self, "device"):
            return

        try:
            self.device.snapshot(directory=self.current_test_info.output_path)
        except Exception:  # pylint: disable=broad-except
            _LOGGER.warning("Unable to take snapshot")

    def test_is_device_component_capable(self):
        """Test case to make sure DUT is a component capable device"""
        asserts.assert_is_instance(
            self.device, component_capable_device.ComponentCapableDevice)

    def test_component_list(self):
        """Test case for component.list()"""
        component_list = self.device.component.list()
        asserts.assert_is_instance(component_list, list)
        asserts.assert_in(ROOT_CM_URL, component_list)

    def test_component_search(self):
        """Test case for component.search()"""
        asserts.assert_true(
            self.device.component.search(name=SYS_MGR_V1_CMX_NAME),
            msg=f"{SYS_MGR_V1_CMX_NAME} component expected to be found")

        asserts.assert_true(
            self.device.component.search(name=ROOT_V2_CM_NAME),
            msg=f"{ROOT_V2_CM_NAME} component expected to be found")

        asserts.assert_false(
            self.device.component.search(name=UNKNOWN_COMPONENT_NAME),
            msg=f"{UNKNOWN_COMPONENT_NAME} component not expected to be found")

    ### TBD - Write a test case for component.launch()


if __name__ == "__main__":
    test_runner.main()
