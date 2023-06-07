#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for Component affordance."""

import logging
from typing import List

from fuchsia_base_test import fuchsia_base_test
from honeydew.interfaces.device_classes import fuchsia_device
from mobly import asserts
from mobly import test_runner

ROOT_CM_URL = "fuchsia-boot:///#meta/root.cm"
SYS_MGR_V1_CMX_NAME = "sysmgr.cmx"
ROOT_V2_CM_NAME = "root.cm"
UNKNOWN_COMPONENT_NAME = "unknown.cm"

_LOGGER: logging.Logger = logging.getLogger(__name__)


class ComponentAffordanceTests(fuchsia_base_test.FuchsiaBaseTest):
    """Component affordance tests"""

    def setup_class(self) -> None:
        """setup_class is called once before running tests.

        It does the following things:
            * Assigns `device` variable with FuchsiaDevice object
        """
        super().setup_class()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

    def test_component_list(self) -> None:
        """Test case for component.list()"""
        if self._is_fuchsia_controller_based_device(self.device):
            with asserts.assert_raises(NotImplementedError):
                self.device.component.list()
            return

        component_list: List[str] = self.device.component.list()
        asserts.assert_is_instance(component_list, list)
        asserts.assert_in(ROOT_CM_URL, component_list)

    def test_component_search(self) -> None:
        """Test case for component.search()"""
        if self._is_fuchsia_controller_based_device(self.device):
            with asserts.assert_raises(NotImplementedError):
                self.device.component.search(name=SYS_MGR_V1_CMX_NAME)
            return

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
