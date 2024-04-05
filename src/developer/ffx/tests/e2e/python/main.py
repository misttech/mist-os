#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Simple FFX host tool E2E test."""

import json
import logging
import subprocess
import time

from mobly import asserts
from mobly import test_runner

from fuchsia_base_test import fuchsia_base_test
from honeydew.interfaces.device_classes import fuchsia_device
import honeydew

_LOGGER: logging.Logger = logging.getLogger(__name__)


class FfxTest(fuchsia_base_test.FuchsiaBaseTest):
    """FFX host tool E2E test."""

    def setup_class(self) -> None:
        """setup_class is called once before running the testsuite."""
        super().setup_class()
        self.dut: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

    def test_component_list(self) -> None:
        """Test `ffx component list` output returns as expected."""
        output = self.dut.ffx.run(["component", "list"])
        asserts.assert_true(
            len(output.splitlines()) > 0,
            f"stdout is unexpectedly empty: {output}",
        )

    def test_get_ssh_address_includes_port(self) -> None:
        """Test `ffx target get-ssh-address` output returns as expected."""
        output = self.dut.ffx.run(["target", "get-ssh-address", "-t", "5"])
        asserts.assert_true(
            ":22" in output, f"expected stdout to contain ':22',got {output}"
        )

    def test_target_show(self) -> None:
        """Test `ffx target show` output returns as expected."""
        output = self.dut.ffx.run(["target", "show", "--json"])
        output_json = json.loads(output)
        got_device_name = output_json[0]["child"][0]["value"]
        # Assert FFX's target show device name matches Honeydew's.
        asserts.assert_equal(got_device_name, self.dut.device_name)

    def test_target_echo_repeat(self) -> None:
        """Test `ffx target echo --repeat` is resilient to daemon failure."""
        process = self.dut.ffx.popen(
            ["target", "echo", "--repeat"], stdout=subprocess.PIPE
        )
        line = process.stdout.readline()
        asserts.assert_true(
            line.startswith(b"SUCCESS"), f"First ping didn't succeed: {line}"
        )
        self.dut.ffx.run(["daemon", "stop"])
        while True:
            line = process.stdout.readline()
            if line.startswith(b"ERROR"):
                break
        line = process.stdout.readline()
        asserts.assert_true(
            line.startswith(b"SUCCESS"),
            f"Success didn't resume after error: {line}",
        )

    # TODO(b/328505123): reenable when `ffx daemon stop` works in builders
    # def test_target_list_without_discovery(self) -> None:
    #     """Test `ffx target list` output returns as expected when discovery is off."""
    #     self.dut.ffx.run(["daemon", "stop", "-t", "1000"])
    #     output = self.dut.ffx.run(
    #         ["--machine", "json", "-c", "ffx.isolated=true", "target", "list"]
    #     )
    #     output_json = json.loads(output)
    #     got_device_name = output_json[0]["nodename"]
    #     # Assert FFX's target list device name matches Honeydew's.
    #     asserts.assert_equal(got_device_name, self.dut.device_name)
    #     # Make sure the daemon hadn't started running
    #     with asserts.assert_raises(honeydew.errors.FfxCommandError):
    #         self.dut.ffx.run(["-c", "daemon.autostart=false", "daemon", "echo"])

    # TODO(b/328505123): reenable when `ffx daemon stop` works in builders
    # def test_target_list_nodename_without_discovery(self) -> None:
    #     """Test `ffx target list <nodename>` output returns as expected when discovery is off."""
    #     self.dut.ffx.run(["daemon", "stop", "-t", "1000"], capture_output=False)
    #     output = self.dut.ffx.run(
    #         [
    #             "--machine",
    #             "json",
    #             "-c",
    #             "ffx.isolated=true",
    #             "target",
    #             "list",
    #             self.dut.device_name,
    #         ]
    #     )
    #     output_json = json.loads(output)
    #     got_device_name = output_json[0]["nodename"]
    #     # Assert FFX's target list device name matches Honeydew's.
    #     asserts.assert_equal(got_device_name, self.dut.device_name)
    #     # Make sure the daemon hadn't started running
    #     with asserts.assert_raises(honeydew.errors.FfxCommandError):
    #         self.dut.ffx.run(["-c", "daemon.autostart=false", "daemon", "echo"])


if __name__ == "__main__":
    test_runner.main()
