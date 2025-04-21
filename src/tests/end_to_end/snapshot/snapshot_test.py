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
import pathlib
import tempfile
import traceback
import zipfile
from typing import Any, Dict, List

import test_data
from fuchsia_base_test import fuchsia_base_test
from honeydew.fuchsia_device.fuchsia_device import FuchsiaDevice
from memory import profile
from mobly import asserts, test_runner
from perf_publish import publish
from perf_utils.utils import FuchsiaPerfResults
from trace_processing import trace_importing, trace_metrics, trace_model
from trace_processing.metrics import cpu

_LOGGER: logging.Logger = logging.getLogger(__name__)
_SNAPSHOT_ZIP = "snapshot_test.zip"
_TEST_SUITE = "fuchsia.test.diagnostics"


class SnapshotPerfResults(FuchsiaPerfResults[None, None]):
    def __init__(self, device: FuchsiaDevice, test_case_path: str):
        self._device = device
        self.test_case_path = test_case_path
        self.test_sub_suite = f"{_TEST_SUITE}.Snapshot".lower()

    def pre_action(self) -> None:
        self._directory = tempfile.TemporaryDirectory()

    def action(self, _: None) -> None:
        self._device.health_check()
        memory_before_snapshot = self._device.ffx.run(["profile", "memory"])
        with open(
            os.path.join(self.test_case_path, "memory_before_snapshot"), "w"
        ) as file:
            file.write(memory_before_snapshot)
        # Gather a performance trace if possible.
        # Some configurations don't support tracing.
        try:
            with self._device.tracing.trace_session(
                categories=[
                    "#default",
                    "gfx",
                    "kernel:meta",
                    "kernel:sched",
                    "magma",
                    "system_metrics",
                    "system_metrics_logger",
                    "memory:kernel",
                ],
                download=True,
                directory=self.test_case_path,
                trace_file="trace.fxt",
                buffer_size=32,
            ):
                self._device.snapshot(self._directory.name, _SNAPSHOT_ZIP)
            memory_after_snapshot = self._device.ffx.run(["profile", "memory"])
            with open(
                os.path.join(self.test_case_path, "memory_after_snapshot"), "w"
            ) as file:
                file.write(memory_after_snapshot)
            # get CPU and memory usage
            path = os.path.join(self.test_case_path, "trace.fxt")
            perf_results = self._get_cpu_results(path)
            perf_results.extend(
                profile.capture_and_compute_metrics(
                    {"feedback": "feedback.cm", "archivist": "archivist.cm"},
                    self._device,
                ).metrics
            )

            perf_path = (
                pathlib.Path(self._directory.name)
                / "snapshot_test_cpu.fuchsiaperf.json"
            )
            for result in perf_results:
                self.additional_json.append(
                    {
                        "test_suite": _TEST_SUITE,
                        "label": result.label,
                        "values": result.values,
                        "unit": result.unit,
                    }
                )
            # NOTE: Metric names need to be lowercase
            trace_metrics.TestCaseResult.write_fuchsiaperf_json(
                perf_results, "fuchsia.snapshot_test_cpu", perf_path
            )
            publish.publish_fuchsiaperf(
                [perf_path], f"{self.test_sub_suite}.txt", test_data
            )
        except:
            _LOGGER.warning(
                "Failed to gather trace. This build may not support it."
            )
            traceback.print_exc()
            self._device.snapshot(self._directory.name, _SNAPSHOT_ZIP)

    def post_action(self, _: None) -> None:
        final_path = os.path.join(self._directory.name, _SNAPSHOT_ZIP)
        with zipfile.ZipFile(final_path) as zf:
            self._validate_inspect(zf)
        self._directory.cleanup()

    def _get_cpu_results(
        self, path: str | os.PathLike[str]
    ) -> list[trace_metrics.TestCaseResult]:
        model: trace_model.Model = (
            trace_importing.create_model_from_trace_file_path(path)
        )
        return list(cpu.CpuMetricsProcessor().process_metrics(model))

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


class SnapshotTest(fuchsia_base_test.FuchsiaBaseTest):
    def setup_class(self) -> None:
        super().setup_class()
        self._fuchsia_device = self.fuchsia_devices[0]
        self._repetitions = self.user_params["repeat_count"]

    def test_snapshot(self) -> None:
        """Get a device snapshot and extract the inspect.json file."""
        SnapshotPerfResults(self._fuchsia_device, self.test_case_path).execute(
            _TEST_SUITE, "Snapshot", self.test_case_path, self._repetitions
        )


if __name__ == "__main__":
    test_runner.main()
