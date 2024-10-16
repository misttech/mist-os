#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Simple FFX host tool E2E test."""

import json
import logging
import subprocess

import honeydew
from fuchsia_base_test import fuchsia_base_test
from honeydew.interfaces.device_classes import fuchsia_device
from mobly import asserts, test_runner

_LOGGER: logging.Logger = logging.getLogger(__name__)


class FfxTest(fuchsia_base_test.FuchsiaBaseTest):
    """FFX host tool E2E test."""

    def setup_class(self) -> None:
        """setup_class is called once before running the testsuite."""
        super().setup_class()
        self.dut: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

    def teardown_test(self) -> None:
        # Until total daemonless functionality is implemented, we must ensure
        # the daemon is running before test teardown, because Lacewing expects
        # the daemon to be running for teardown operations. Previously the
        # daemon was automatically restarted by `ffx target wait`, but this is
        # now no longer the case because the command does not use the daemon
        # anymore.
        self.dut.ffx.run(["daemon", "start", "--background"])
        super().teardown_test()

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
        output = self.dut.ffx.get_target_information()
        got_device_name = output.target.name
        # Assert FFX's target show device name matches Honeydew's.
        asserts.assert_equal(got_device_name, self.dut.device_name)

    def test_target_echo_repeat(self) -> None:
        """Test `ffx target echo --repeat` is resilient to daemon failure."""
        with self.dut.ffx.popen(
            ["target", "echo", "--repeat"],
            stdout=subprocess.PIPE,
            text=False,
        ) as process:
            try:
                line = process.stdout.readline()
                asserts.assert_true(
                    line.startswith(b"SUCCESS"),
                    f"First ping didn't succeed: {line}",
                )
                self.dut.ffx.run(["daemon", "stop"])
                while True:
                    line = process.stdout.readline()
                    if not line.startswith(b"ERROR") and not line.startswith(
                        b"Waiting for"
                    ):
                        break
                    print(line)
                asserts.assert_true(
                    line.startswith(b"SUCCESS"),
                    f"Success didn't resume after error: {line}",
                )
            finally:
                process.kill()

    # Note: in this test we do _not_ want to probe the device, since we will try
    # to probe every device visible in the builder. But in EngProd environments,
    # that could be dozens of devices
    # TODO(b/355292969): re-enable when client-side discovery is re-enabled (see libtarget::is_discover_enabled())
    def _test_target_list_without_discovery(self) -> None:
        """Test `ffx target list` output returns as expected when discovery is off."""
        self.dut.ffx.run(["daemon", "stop"])
        output = self.dut.ffx.run(
            [
                "--machine",
                "json",
                "-c",
                "ffx.isolated=true",
                "target",
                "list",
            ]
        )
        output_json = json.loads(output)
        devices = [
            o for o in output_json if o["nodename"] == self.dut.device_name
        ]
        # Assert ffx's target list device name contain's Honeydew's device.
        asserts.assert_greater(len(devices), 0)
        # Assert that we are not probing the device to identify the RCS state
        asserts.assert_equal(devices[0]["rcs_state"], "N")
        # Assert that we are not probing the device to identify the type
        asserts.assert_equal(devices[0]["target_type"], "Unknown")
        with asserts.assert_raises(honeydew.errors.FfxCommandError):
            self.dut.ffx.run(["-c", "daemon.autostart=false", "daemon", "echo"])

    # TODO(b/355292969): re-enable when client-side discovery is re-enabled (see libtarget::is_discover_enabled())
    def _test_target_list_nodename_without_discovery(self) -> None:
        """Test `ffx target list <nodename>` output returns as expected when discovery is off."""
        self.dut.ffx.run(["daemon", "stop"], capture_output=False)
        output = self.dut.ffx.run(
            [
                "--machine",
                "json",
                "-c",
                "ffx.isolated=true",
                "-c",
                "ffx.target-list.local-connect=true",
                "target",
                "list",
                self.dut.device_name,
            ]
        )
        output_json = json.loads(output)
        devices = [
            o for o in output_json if o["nodename"] == self.dut.device_name
        ]
        # Assert Honeydew's device is the only device returned.
        asserts.assert_equal(len(devices), 1)
        # Assert that we can correctly identify the RCS state
        asserts.assert_equal(devices[0]["rcs_state"], "Y")
        # Assert that we can correctly identify the product
        asserts.assert_not_equal(devices[0]["target_type"], "Unknown")

        # Make sure the daemon hadn't started running
        with asserts.assert_raises(honeydew.errors.FfxCommandError):
            self.dut.ffx.run(["-c", "daemon.autostart=false", "daemon", "echo"])

    def run_ffx(self, args: list[str]) -> str:
        """Run ffx in the specific way we need, not the standard Honeydew way"""
        config = self.dut.ffx.config
        cmd = [
            config.binary_path,
            "--isolate-dir",
            config.isolate_dir.directory(),
        ] + args
        return subprocess.check_output(cmd, stderr=subprocess.STDOUT).decode()

    def test_local_discovery(self) -> None:
        """Test that we can resolve a target locally"""
        # Let's make sure the CLI believes that discovery is turned off,
        # by setting ffx.isolated=true
        cmd = [
            "-c",
            "ffx.isolated=true",
            "-t",
            f"{self.dut.ffx._target}",
            "target",
            "echo",
        ]
        output = self.run_ffx(cmd)
        # Unfortunately we're not checking _that_ this is being resolved locally.
        # To do that we'd probably want to run a test in which the daemon isn't
        # running, but honeydew isn't set up for that.
        asserts.assert_equal(output, 'SUCCESS: received "Ffx"\n')

    def test_wait_with_local_discovery(self) -> None:
        """Test that we can wait for a target when daemon discovery is disabled"""
        # Let's make sure the CLI believes that discovery is turned off,
        # by setting ffx.isolated=true
        cmd = [
            "-c",
            "ffx.isolated=true",
            "-t",
            f"{self.dut.ffx._target}",
            "target",
            "wait",
        ]
        output = self.run_ffx(cmd)
        asserts.assert_equal(output, "")


if __name__ == "__main__":
    test_runner.main()
