#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""E2E test for diagnostics functionality.

Test that asserts that we can read logs and Inspect.
"""

import json
import logging
from typing import Any, Dict, List

from fuchsia_base_test import fuchsia_base_test
from honeydew.transports.ffx.ffx import FFX
from mobly import asserts, test_runner
from perf_utils.utils import FuchsiaPerfResults

_LOGGER: logging.Logger = logging.getLogger(__name__)
_TEST_SUITE = "fuchsia.test.diagnostics"


class LogPerfResults(FuchsiaPerfResults[None, str]):
    def __init__(self, ffx: FFX):
        self._ffx = ffx

    def pre_action(self) -> None:
        return None

    def action(self, _: None) -> str:
        return self._ffx.run(
            cmd=["--machine", "json", "log", "--symbolize", "off", "dump"],
            log_output=False,
        )

    def post_action(self, step_output: str) -> None:
        asserts.assert_greater(len(step_output), 0)


class InspectPerfResults(FuchsiaPerfResults[None, str]):
    def __init__(self, ffx: FFX):
        self._ffx = ffx

    def pre_action(self) -> None:
        return None

    def action(self, _: None) -> str:
        return self._ffx.run(
            cmd=["--machine", "json", "inspect", "show"], log_output=False
        )

    def post_action(self, result: str) -> None:
        inspect_data: list[Any] = json.loads(result)
        asserts.assert_greater(len(inspect_data), 0)
        self._check_archivist_data(inspect_data)

    def _check_archivist_data(self, inspect_data: List[Dict[str, Any]]) -> None:
        """Find the Archivist's data, and assert that it's status is 'OK'."""
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


class DiagnosticsTest(fuchsia_base_test.FuchsiaBaseTest):
    def setup_class(self) -> None:
        super().setup_class()
        self._fuchsia_device = self.fuchsia_devices[0]
        self._repeat_count: int = self.user_params["repeat_count"]

    def test_inspect(self) -> None:
        """Validates that we can snapshot Inspect from the device."""
        InspectPerfResults(self._fuchsia_device.ffx).execute(
            _TEST_SUITE, "Inspect", self.test_case_path, self._repeat_count
        )

    def test_logs(self) -> None:
        """Validates that we can snapshot logs from the device."""
        LogPerfResults(self._fuchsia_device.ffx).execute(
            _TEST_SUITE, "Logs", self.test_case_path, self._repeat_count
        )


if __name__ == "__main__":
    test_runner.main()
