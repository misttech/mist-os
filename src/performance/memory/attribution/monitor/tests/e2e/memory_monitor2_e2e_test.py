# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Tests `ffx profile memory component` integration with `memory_monitor2` and attribution
principals. Also verifies other protocol exposed by `memory_monitor2`

The test it verifies the features are availability, but does not verify the data.
"""
import json
import re
import time
from pathlib import Path

from fuchsia_base_test import fuchsia_base_test
from honeydew.interfaces.device_classes import fuchsia_device
from mobly import asserts, test_runner
from trace_processing import trace_importing, trace_model, trace_utils


def assertContainsRegex(reg_str: str, content: str) -> None:
    asserts.assert_true(
        re.search(reg_str, content),
        msg=f"The text content (len={len(content)}) does not contain any occurrence of regex: {reg_str}\n"
        f"content[:256] = {content[:256]}",
    )


class MemoryMonitor2EndToEndTest(fuchsia_base_test.FuchsiaBaseTest):
    def setup_class(self) -> None:
        """setup_class is called once before running tests."""
        super().setup_class()
        self.dut: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]
        self.dut.ffx.run(
            ["config", "set", "ffx_profile_memory_components", "true"]
        )

    def test_ffx_memory_component_without_args(self) -> None:
        profile = self.dut.ffx.run(
            ["profile", "memory", "components"], log_output=False
        )
        with open(
            Path(self.test_case_path) / "profile_memory_components.txt", "wt"
        ) as out:
            out.write(profile)

        # Verifies that some data is produced.
        assertContainsRegex(r"(?m)^Total memory: \d+\.\d+ MiB$", profile)
        assertContainsRegex(r"(?m)^Kernel: +\d+\.\d+ MiB$", profile)
        assertContainsRegex(
            r"(?m)^Processes: memory_monitor2\.cm \(\d+\)$", profile
        )

    def test_memory_monitor2_inspect(self) -> None:
        inspect_json = self.dut.ffx.run(
            ["--machine", "json", "inspect", "show", "core/memory_monitor2"]
        )
        with open(Path(self.test_case_path) / "inspect_show.json", "wt") as out:
            out.write(inspect_json)
        inspect_list = json.loads(inspect_json)
        (only_entry,) = inspect_list

        # Verifies that some data is produced.
        # There should be at least one assertion per lazy node to detect failures and timeouts.
        asserts.assert_equal(only_entry["moniker"], "core/memory_monitor2")

        root = only_entry["payload"]["root"]
        asserts.assert_greater(root["kmem_stats"]["total_bytes"], 0)
        asserts.assert_greater(
            root["kmem_stats_compression"]["pages_decompressed_unit_ns"], 0
        )

    def test_memory_monitor2_inspect2(self) -> None:
        inspect_col = self.dut.inspect.get_data(
            monikers=["core/memory_monitor2"]
        )
        (only_entry,) = inspect_col.data

        # Verifies that some data is produced.
        # There should be at least one assertion per lazy node to detect failures and timeouts.
        asserts.assert_equal(only_entry.moniker, "core/memory_monitor2")
        if only_entry.payload is None:
            raise AssertionError("Payload should not be none")
        root = only_entry.payload["root"]
        asserts.assert_greater(root["kmem_stats"]["total_bytes"], 0)
        asserts.assert_greater(
            root["kmem_stats_compression"]["pages_decompressed_unit_ns"], 0
        )

    def test_memory_monitor2_traces_provider(self) -> None:
        json_text = self.dut.ffx.run(
            ["--machine", "json", "trace", "list-providers"]
        )
        with open(
            Path(self.test_case_path) / "trace_list-providers.json", "wt"
        ) as out:
            out.write(json_text)
        providers = json.loads(json_text)
        asserts.assert_in(
            "memory_monitor2.cm", [prov["name"] for prov in providers]
        )

    def test_memory_monitor2_traces_collect(self) -> None:
        CATEGORY = "memory:kernel"
        trace_path = Path(self.test_case_path) / "trace.fxt"
        with self.dut.tracing.trace_session(
            categories=[CATEGORY],
            download=True,
            directory=str(trace_path.parent),
            trace_file=trace_path.name,
        ):
            # Events are logged every seconds. It is not very nice to have to wait a given amount
            # of time. If that proves brittle, we should fallback on a larger value.
            time.sleep(4)

        json_trace_file: str = trace_importing.convert_trace_file_to_json(
            trace_path
        )
        model: trace_model.Model = trace_importing.create_model_from_file_path(
            json_trace_file
        )
        event_names = {
            event.name
            for event in trace_utils.filter_events(
                model.all_events(),
                category=CATEGORY,
                type=trace_model.Event,
            )
        }
        asserts.assert_equal(
            event_names,
            {
                "kmem_stats_a",
                "kmem_stats_b",
                "kmem_stats_compression",
                "kmem_stats_compression_time",
            },
        )


if __name__ == "__main__":
    test_runner.main()
