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
import unittest
from pathlib import Path

from fuchsia_base_test import fuchsia_base_test
from honeydew.fuchsia_device import fuchsia_device
from honeydew.transports.ffx import errors as ffx_errors
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

    def write_output(self, cmd_output: str) -> None:
        """Writes the command output to a dedicated file for investigation."""
        with open(
            Path(self.test_case_path) / "ffx_command_stdout_and_stderr.txt",
            "wt",
        ) as out:
            out.write(cmd_output)

    def test_ffx_profile_memory_component_without_args(self) -> None:
        profile = self.dut.ffx.run(
            ["profile", "memory", "components"], log_output=False
        )
        self.write_output(profile)

        # Verifies that some data is produced.
        assertContainsRegex(r"(?m)^Total memory: \d+\.\d+ MiB$", profile)
        assertContainsRegex(r"(?m)^Kernel: +\d+\.\d+ MiB$", profile)
        assertContainsRegex(
            r"(?m)^\s*Processes:\s*memory_monitor2\.cm \(\d+\)\s*$", profile
        )

    def test_ffx_profile_memory_component_with_json_output(self) -> None:
        cmd_output = self.dut.ffx.run(
            ["--machine", "json", "profile", "memory", "components"],
            log_output=False,
        )
        self.write_output(cmd_output)
        # Remove `Resource %d not found` line from the output.
        # TODO(b/409272413): simplify this code when stdio and stderr are no longer aggregated.
        cmd_output = "\n".join(
            l for l in cmd_output.split("\n") if not l.startswith("Resource ")
        )

        profile = json.loads(cmd_output)
        (mm2,) = [
            p
            for p in profile["principals"]
            if p["name"] == "core/memory_monitor2"
        ]
        asserts.assert_in("processes", mm2)
        asserts.assert_in("vmos", mm2)

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
        asserts.assert_greater_equal(
            root["kmem_stats_compression"]["pages_decompressed_unit_ns"], 0
        )

    def test_memory_monitor2_inspect2(self) -> None:
        inspect_col = self.dut.get_inspect_data(
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
        asserts.assert_greater_equal(
            root["kmem_stats_compression"]["pages_decompressed_unit_ns"], 0
        )

    def test_memory_monitor2_inspect_current(self) -> None:
        inspect_col = self.dut.get_inspect_data(
            monikers=["core/memory_monitor2"]
        )
        (only_entry,) = inspect_col.data

        # Verify that a current memory capture is present, with 6 fields.
        asserts.assert_equal(only_entry.moniker, "core/memory_monitor2")
        if only_entry.payload is None:
            raise AssertionError("Payload should not be none")
        root = only_entry.payload["root"]
        value_dict = root["current"]["core/memory_monitor2"]
        for field in (
            "committed_private",
            "committed_scaled",
            "committed_total",
            "populated_private",
            "populated_scaled",
            "populated_total",
        ):
            asserts.assert_in(field, value_dict)
            asserts.assert_greater(value_dict[field], 0)

    def test_memory_monitor2_is_in_traces_provider(self) -> None:
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

    def test_profile_memory_with_monitor2_report(self) -> None:
        profile = self.dut.ffx.run(
            [
                "profile",
                "memory",
                "--backend",
                "memory_monitor_2",
            ],
            log_output=False,
        )
        # Verifies that the report comes from memory_monitor2.
        assertContainsRegex(r"(?m)^ Principal name:", profile)

    def test_ffx_profile_memory_with_json_output(self) -> None:
        cmd_output = self.dut.ffx.run(
            [
                "--machine",
                "json",
                "profile",
                "memory",
                "--backend",
                "memory_monitor_2",
            ],
            log_output=False,
        )
        self.write_output(cmd_output)
        # Remove `Resource %d not found` line from the output.
        # TODO(b/409272413): simplify this code when stdio and stderr are no longer aggregated.
        cmd_output = "\n".join(
            l for l in cmd_output.split("\n") if not l.startswith("Resource ")
        )
        # Assert that this is a ComponentDigest
        profile = json.loads(cmd_output)["ComponentDigest"]
        # Assert that is has a principal for memory monitor 2.
        (principal,) = [
            p
            for p in profile["principals"]
            if p["name"] == "core/memory_monitor2"
        ]
        asserts.assert_in("processes", principal)
        asserts.assert_in("vmos", principal)

    def test_profile_memory_with_monitor2_incompatible_args(self) -> None:
        INCOMPATIBLE_ARGS_LIST: list[list[str]] = [
            ["--process_koids", "123"],
            ["--process_names", "123"],
            ["--interval", "123"],
            ["--buckets"],
            ["--undigested"],
            ["--exact_sizes"],
        ]
        for incompatible_args in INCOMPATIBLE_ARGS_LIST:
            with unittest.TestCase.assertRaises(
                self, ffx_errors.FfxCommandError
            ):
                self.dut.ffx.run(
                    [
                        "profile",
                        "memory",
                        "--backend",
                        "memory_monitor_2",
                    ]
                    + incompatible_args,
                    log_output=False,
                )


if __name__ == "__main__":
    test_runner.main()
