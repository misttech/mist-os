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

from typing import Dict, Any, List
from mobly import asserts, test_runner
from fuchsia_base_test import fuchsia_base_test

_LOGGER: logging.Logger = logging.getLogger(__name__)


class SnapshotTest(fuchsia_base_test.FuchsiaBaseTest):
    def setup_class(self) -> None:
        """Initialize all DUT(s)"""
        super().setup_class()
        self.fuchsia_dut = self.fuchsia_devices[0]

    def test_snapshot(self) -> None:
        # Get a device snapshot and extract the inspect.json file.
        time_ns: list[int] = []
        for _ in range(self.user_params["repeat_count"]):
            with tempfile.TemporaryDirectory() as td:
                file_name = "snapshot_test.zip"
                start = time.monotonic_ns()
                self.fuchsia_dut.snapshot(td, file_name)
                dur = time.monotonic_ns() - start
                time_ns.append(dur)

                final_path = os.path.join(td, file_name)
                stat = os.stat(final_path)
                _LOGGER.info(
                    "Snapshot is %d bytes, took %.3f seconds",
                    stat.st_size,
                    (dur / 1e9),
                )

                with zipfile.ZipFile(final_path) as zf:
                    # IMPORTANT: the exact contents of these files should not be verified. We just
                    # want to assert that they exist and are not empty.
                    self._validate_non_empty_files(
                        zf,
                        [
                            "annotations.json",
                            "build.snapshot.xml",
                            "log.system.txt",
                            "log.kernel.txt",
                            "metadata.json",
                        ],
                    )
                    self._validate_inspect(zf)
        self._record_fuchsiaperf(time_ns)

    def _validate_non_empty_files(
        self, zf: zipfile.ZipFile, filenames: List[str]
    ) -> None:
        for filename in filenames:
            with zf.open(filename) as f:
                contents = f.read()
                asserts.assert_greater(len(contents), 0)

    def _validate_inspect(self, zf: zipfile.ZipFile) -> None:
        with zf.open("inspect.json") as inspect_file:
            inspect_data = json.load(inspect_file)
        asserts.assert_greater(len(inspect_data), 0)
        self._check_archivist_data(inspect_data)

    def _check_archivist_data(self, inspect_data: List[Dict[str, Any]]) -> None:
        # Find the Archivist's data, and assert that it's status is "OK"
        archivist_only: List[Dict[str, Any]] = [
            data
            for data in inspect_data
            if data.get("moniker") == "bootstrap/archivist"
        ]
        asserts.assert_equal(
            len(archivist_only),
            1,
            "Expected to find one Archivist in the Inspect output.",
        )
        archivist_data = archivist_only[0]

        health = archivist_data["payload"]["root"]["fuchsia.inspect.Health"]
        asserts.assert_equal(
            health["status"],
            "OK",
            "Archivist did not return OK status",
        )

    def _record_fuchsiaperf(self, time_ns: list[int]) -> None:
        fuchsiaperf_data = [
            {
                "test_suite": "fuchsia.test.snapshot",
                "label": "SnapshotDuration",
                "values": time_ns,
                "unit": "ns",
            },
        ]
        test_perf_file = os.path.join(self.log_path, "results.fuchsiaperf.json")
        with open(test_perf_file, "w") as f:
            json.dump(fuchsiaperf_data, f, indent=4)


if __name__ == "__main__":
    test_runner.main()
