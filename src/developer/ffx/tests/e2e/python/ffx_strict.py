#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Simple FFX host tool E2E test."""

import inspect
import json
import logging
import os
import subprocess
from typing import List, Text, Tuple

import ffxtestcase
from honeydew.errors import FfxCommandError
from mobly import asserts, test_runner

_LOGGER: logging.Logger = logging.getLogger(__name__)


def build_strict_config_args(
    ssh_pub_key: Text, ssh_private_key: Text
) -> List[Text]:
    environ = os.environ
    config = "--config"
    retval = []
    retval.append(config)
    out_dir = os.environ.get("TEST_UNDECLARED_OUTPUTS_DIR")
    caller_frame = inspect.currentframe()
    if caller_frame:
        caller_frame = caller_frame.f_back
    if out_dir and caller_frame:
        out_dir = os.path.join(out_dir, f"{caller_frame.f_code.co_name}.log")
    if not out_dir:
        out_dir = "/dev/null"
    _LOGGER.info(f"Setting ffx config log dir to {out_dir}")
    retval.append(f"test.output_path={out_dir}")
    retval.append(config)
    retval.append(f"ssh.pub={ssh_pub_key}")
    retval.append(config)
    retval.append(f"ssh.priv={ssh_private_key}")
    retval.append(config)
    retval.append(
        f"fastboot.devices_file.path={environ['HOME']}/.fastboot/devices"
    )
    retval.append(config)
    retval.append(f"log.dir={environ['FUCHSIA_TEST_OUTDIR']}/ffx_logs")
    return retval


class FfxStrictTest(ffxtestcase.FfxTestCase):
    """FFX host tool E2E test For Strict."""

    def setup_class(self) -> None:
        # This just gets some things out of the way before we start turning
        # the daemon off and on again.
        super().setup_class()
        self.dut_ssh_address = self.dut.ffx.get_target_ssh_address()
        self.dut_name = self.dut.ffx.get_target_name()

    def setup_test(self) -> None:
        """Each test must run without the daemon."""
        super().setup_test()
        self.dut.ffx.run(["daemon", "stop"])

    def _get_ssh_key_information(self) -> Tuple[Text, Text]:
        ssh_pub_output = json.loads(
            self.run_ffx(["config", "get", "-s", "first", "ssh.pub"])
        )
        if isinstance(ssh_pub_output, List):
            print("pub is list")
            ssh_pub = ssh_pub_output[0].strip().replace('"', "")
        elif isinstance(ssh_pub_output, Text):
            ssh_pub = ssh_pub_output.strip().replace('"', "")

        print(ssh_pub)

        ssh_priv_output = json.loads(
            self.run_ffx(["config", "get", "-s", "first", "ssh.priv"])
        )
        ssh_priv = ""
        if isinstance(ssh_priv_output, List):
            print("priv is List")
            ssh_priv = ssh_priv_output[0].strip().replace('"', "")
        elif isinstance(ssh_priv_output, Text):
            ssh_priv = ssh_priv_output.strip().replace('"', "")

        print(ssh_priv)

        return (ssh_pub, ssh_priv)

    def test_target_echo_no_start_daemon(self) -> None:
        """Test `ffx --strict target echo` does not affect daemon state."""
        ssh_args = self._get_ssh_key_information()
        output = self.run_ffx(
            [
                "--strict",
                "-t",
                f"{self.dut_ssh_address}",
                "--machine",
                "json",
                "-o",
                "/dev/null",
                *build_strict_config_args(*ssh_args),
                "target",
                "echo",
                "From a Test",
            ]
        )
        output_json = json.loads(output)

        asserts.assert_equal(
            output_json, {"message": 'SUCCESS: received "From a Test"'}
        )
        with asserts.assert_raises(FfxCommandError):
            self.dut.ffx.run(["-c", "daemon.autostart=false", "daemon", "echo"])

    def test_strict_errors_with_target_name(self) -> None:
        """Test `ffx --strict target echo` fails when attempt discovery."""
        ssh_args = self._get_ssh_key_information()
        with asserts.assert_raises(subprocess.CalledProcessError):
            self.run_ffx(
                [
                    "--strict",
                    "-t",
                    f"{self.dut_name}",
                    "--machine",
                    "json",
                    "-o",
                    "/dev/null",
                    *build_strict_config_args(*ssh_args),
                    "target",
                    "echo",
                    "From a Test",
                ]
            )


if __name__ == "__main__":
    test_runner.main()
