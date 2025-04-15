# Copyright 2025 The Fuchsia Authors. All rights reserved.
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
from honeydew.fuchsia_device import fuchsia_device
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

    def write_output(self, cmd_output: str, filename: str) -> None:
        """Writes the command output to a dedicated file for investigation."""
        with open(
            Path(self.test_case_path) / filename,
            "wt",
        ) as out:
            out.write(cmd_output)

    def test_memory_monitor2_is_in_traces_provider(self) -> None:
        json_text = self.dut.ffx.run(
            ["--machine", "json-pretty", "trace", "list-providers"]
        )
        self.write_output(json.dumps(json_text), "trace_list-providers.json")
        providers = json.loads(json_text)
        asserts.assert_in(
            "memory_monitor2.cm", [prov["name"] for prov in providers]
        )

    def test_memory_traces_content_collect(self) -> None:
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
                "memory_stall",
            },
        )


if __name__ == "__main__":
    test_runner.main()
