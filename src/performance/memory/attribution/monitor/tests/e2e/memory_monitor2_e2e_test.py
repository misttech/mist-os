# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Tests `ffx profile memory component` integration with `memory_monitor2` and attribution
principals. Also verifies other protocol exposed by `memory_monitor2`

The test it verifies the features are availability, but does not verify the data.
"""
import json
import re
import unittest
from pathlib import Path

from fuchsia_base_test import fuchsia_base_test
from honeydew.fuchsia_device import fuchsia_device
from honeydew.transports.ffx import errors as ffx_errors
from mobly import asserts, test_runner


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

    def write_output(self, cmd_output: str, filename: str) -> None:
        """Writes the command output to a dedicated file for investigation."""
        with open(
            Path(self.test_case_path) / filename,
            "wt",
        ) as out:
            out.write(cmd_output)

    def test_ffx_profile_memory_component_without_args(self) -> None:
        profile = self.dut.ffx.run(
            ["profile", "memory", "components"], log_output=False
        )
        self.write_output(profile, "profile_memory_components.txt")

        # Verifies that some data is produced.
        assertContainsRegex(r"(?m)^Total memory: \d+\.\d+ MiB$", profile)
        assertContainsRegex(r"(?m)^Kernel: +\d+\.\d+ MiB$", profile)
        assertContainsRegex(
            r"(?m)^\s*Processes:\s*memory_monitor2\.cm \(\d+\)\s*$", profile
        )

    def test_ffx_profile_memory_component_with_json_output(self) -> None:
        cmd_output = self.dut.ffx.run(
            ["--machine", "json-pretty", "profile", "memory", "components"],
            log_output=False,
        )
        self.write_output(cmd_output, "profile_memory_components.json")
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
            [
                "--machine",
                "json-pretty",
                "inspect",
                "show",
                "core/memory_monitor2",
            ]
        )
        self.write_output(inspect_json, "inspect_show.json")

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
                "json-pretty",
                "profile",
                "memory",
                "--backend",
                "memory_monitor_2",
            ],
            log_output=False,
        )
        self.write_output(cmd_output, "profile_memory.json")
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
