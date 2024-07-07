#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for Inspect affordance."""

import logging
from typing import Any

from fuchsia_base_test import fuchsia_base_test
from mobly import asserts, test_runner

from honeydew.interfaces.device_classes import fuchsia_device

_LOGGER: logging.Logger = logging.getLogger(__name__)


class InspectAffordanceTests(fuchsia_base_test.FuchsiaBaseTest):
    """Inspect affordance tests"""

    def setup_class(self) -> None:
        super().setup_class()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

    def test_get_data_without_selectors_and_monikers(self) -> None:
        inspect_data: list[dict[str, Any]] = self.device.inspect.get_data()

        asserts.assert_is_instance(inspect_data, list)
        asserts.assert_is_not_none(inspect_data)

    def test_get_data_with_one_selector_validate_schema(self) -> None:
        class _AnyUnsignedInteger:
            def __eq__(self, other: object) -> bool:
                return isinstance(other, int) and other >= 0

        inspect_data: list[dict[str, Any]] = self.device.inspect.get_data(
            selectors=["bootstrap/archivist:root/fuchsia.inspect.Health"],
        )

        asserts.assert_is_instance(inspect_data, list)
        asserts.assert_equal(len(inspect_data), 1)
        version: int = inspect_data[0]["version"]
        asserts.assert_equal(
            version, 1, msg=f"Expected version to be 1 but got {version}"
        )

        expected: list[dict[str, Any]] = [
            {
                "data_source": "Inspect",
                "metadata": {
                    "component_url": "fuchsia-boot:///archivist#meta/archivist.cm",
                    "timestamp": _AnyUnsignedInteger(),
                },
                "moniker": "bootstrap/archivist",
                "payload": {
                    "root": {
                        "fuchsia.inspect.Health": {
                            "start_timestamp_nanos": _AnyUnsignedInteger(),
                            "status": "OK",
                        }
                    }
                },
                "version": 1,
            }
        ]
        asserts.assert_equal(
            inspect_data,
            expected,
            f"Expected: ####{expected}#### but received: ####{inspect_data}####",
        )

    def test_get_data_with_multiple_selectors(self) -> None:
        selectors: list[str] = [
            "core/system-update/system-update-checker",
            "bootstrap/archivist",
        ]

        inspect_data: list[dict[str, Any]] = self.device.inspect.get_data(
            selectors=selectors,
        )

        asserts.assert_is_instance(inspect_data, list)
        asserts.assert_equal(len(inspect_data), 2)

        for inspect_node in inspect_data:
            asserts.assert_true(
                inspect_node["moniker"] in selectors,
                msg="",
            )


if __name__ == "__main__":
    test_runner.main()
