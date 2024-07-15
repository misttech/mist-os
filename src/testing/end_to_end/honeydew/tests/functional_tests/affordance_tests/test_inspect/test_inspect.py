#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for Inspect affordance."""

import logging
from typing import Any

import fuchsia_inspect
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
        inspect_data_collection: fuchsia_inspect.InspectDataCollection = (
            self.device.inspect.get_data()
        )

        asserts.assert_is_instance(
            inspect_data_collection, fuchsia_inspect.InspectDataCollection
        )
        for inspect_data in inspect_data_collection.data:
            asserts.assert_is_instance(
                inspect_data,
                fuchsia_inspect.InspectData,
            )

    def test_get_data_with_one_selector_validate_schema(self) -> None:
        class _AnyUnsignedInteger:
            def __eq__(self, other: object) -> bool:
                return isinstance(other, int) and other >= 0

        inspect_data_collection: fuchsia_inspect.InspectDataCollection = (
            self.device.inspect.get_data(
                selectors=["bootstrap/archivist:root/fuchsia.inspect.Health"],
            )
        )

        expected_payload: dict[str, Any] = {
            "root": {
                "fuchsia.inspect.Health": {
                    "start_timestamp_nanos": _AnyUnsignedInteger(),
                    "status": "OK",
                }
            }
        }
        fuchsia_inspect_health_data: fuchsia_inspect.InspectData = (
            inspect_data_collection.data[0]
        )
        asserts.assert_equal(
            fuchsia_inspect_health_data.payload,
            expected_payload,
            f"Expected payload: ####{expected_payload}#### but "
            f"received payload: ####{fuchsia_inspect_health_data.payload}####",
        )

    def test_get_data_with_multiple_selectors(self) -> None:
        selectors: list[str] = [
            "core/system-update/system-update-checker",
            "bootstrap/archivist",
        ]

        inspect_data_collection: fuchsia_inspect.InspectDataCollection = (
            self.device.inspect.get_data(
                selectors=selectors,
            )
        )

        monikers: list[str] = [
            inspect_data.moniker
            for inspect_data in inspect_data_collection.data
        ]
        asserts.assert_equal(sorted(monikers), sorted(selectors))


if __name__ == "__main__":
    test_runner.main()
