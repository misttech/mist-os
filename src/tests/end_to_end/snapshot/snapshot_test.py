#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""E2E test for Fuchsia snapshot functionality

Test that Fuchsia snapshots include Inspect data for Archivist, and
that Archivist is OK.
"""

import json
import logging
import os
import tempfile
import time
import typing
import zipfile

from mobly import test_runner
from mobly.asserts import assert_equal
from fuchsia_base_test import fuchsia_base_test

_LOGGER: logging.Logger = logging.getLogger(__name__)


class SnapshotTest(fuchsia_base_test.FuchsiaBaseTest):  # type: ignore[misc]
    def setup_class(self) -> None:
        """Initialize all DUT(s)"""
        super().setup_class()
        self.fuchsia_dut = self.fuchsia_devices[0]

    def test_snapshot(self) -> None:
        # Get a device snapshot and extract the inspect.json file.
        for _ in range(self.user_params["repeat_count"]):
            with tempfile.TemporaryDirectory() as td:
                file_name = "snapshot_test.zip"
                start = time.time()
                self.fuchsia_dut.snapshot(td, file_name)
                dur = time.time() - start
                final_path = os.path.join(td, file_name)
                stat = os.stat(final_path)

                _LOGGER.info(
                    "Snapshot is %d bytes, took %.3f seconds", stat.st_size, dur
                )
                with zipfile.ZipFile(final_path) as zf:
                    with zf.open("inspect.json") as inspect_file:
                        inspect_data = json.load(inspect_file)
                        self._check_archivist_data(inspect_data)

    def _check_archivist_data(
        self, inspect_data: typing.List[typing.Dict[str, typing.Any]]
    ) -> None:
        # Find the Archivist's data, and assert that it's status is "OK"
        archivist_only: typing.List[typing.Dict[str, typing.Any]] = [
            data
            for data in inspect_data
            if data.get("moniker") == "bootstrap/archivist"
        ]
        assert_equal(
            len(archivist_only),
            1,
            "Expected to find one Archivist in the Inspect output.",
        )
        archivist_data = archivist_only[0]

        health = archivist_data["payload"]["root"]["fuchsia.inspect.Health"]
        assert_equal(
            health["status"],
            "OK",
            "Archivist did not return OK status",
        )


if __name__ == "__main__":
    test_runner.main()
