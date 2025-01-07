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
from typing import Any, List, Optional, Text

import ffxtestcase
from honeydew.errors import FfxCommandError
from mobly import asserts, test_runner

_LOGGER: logging.Logger = logging.getLogger(__name__)


class FfxStrictTest(ffxtestcase.FfxTestCase):
    """FFX host tool E2E test For Strict."""

    def setup_class(self) -> None:
        # This just gets some things out of the way before we start turning
        # the daemon off and on again.
        super().setup_class()
        self.dut_ssh_address = self.dut.ffx.get_target_ssh_address()
        self.dut_name = self.dut.ffx.get_target_name()
        self.ssh_private_key: Optional[str] = None

    def setup_test(self) -> None:
        """Each test must run without the daemon."""
        super().setup_test()
        self.dut.ffx.run(["daemon", "stop"])

    # Return list of ["key=val"]
    def _get_configs(self, keys: List[str]) -> List[str]:
        outputs = []
        for key in keys:
            output = json.loads(
                self.run_ffx(["config", "get", "-s", "first", key])
            )
            asserts.assert_true(
                isinstance(output, Text),
                f"Value for {key} is not a string: {output}",
            )
            output = output.strip().replace('"', "")
            outputs.append(f"{key}={output}")
        return outputs

    # Look up and store the user's private key
    def _get_ssh_private_key(self) -> None:
        if self.ssh_private_key:
            return
        ssh_priv_output = json.loads(
            self.run_ffx(["config", "get", "-s", "first", "ssh.priv"])
        )
        ssh_priv = ""
        if isinstance(ssh_priv_output, List):
            ssh_priv = ssh_priv_output[0].strip().replace('"', "")
        elif isinstance(ssh_priv_output, Text):
            ssh_priv = ssh_priv_output.strip().replace('"', "")

        self.ssh_private_key = ssh_priv

    # Build the default configs passed to strict invocations of ffx
    def _build_strict_config_args(self, extra_configs: List[str]) -> List[Text]:
        environ = os.environ
        configs = extra_configs
        # Get output directory
        out_dir = os.environ.get("TEST_UNDECLARED_OUTPUTS_DIR")
        caller_frame = inspect.currentframe()
        if caller_frame:
            caller_frame = caller_frame.f_back
        if out_dir and caller_frame:
            out_dir = os.path.join(
                out_dir, f"{caller_frame.f_code.co_name}.log"
            )
        if not out_dir:
            out_dir = "/dev/null"
        _LOGGER.info(f"Setting ffx config log dir to {out_dir}")
        # Get other required configs
        configs.append(f"test.output_path={out_dir}")
        self._get_ssh_private_key()
        configs.append(f"ssh.priv={self.ssh_private_key}")
        configs.append(
            f"fastboot.devices_file.path={environ['HOME']}/.fastboot/devices"
        )
        configs.append(f"log.dir={environ['FUCHSIA_TEST_OUTDIR']}/ffx_logs")
        # Return as list of args: ["-c, "key1=val1", "-c", "key2=val2", ...]
        retval = []
        for c in configs:
            retval.append("--config")
            retval.append(c)
        return retval

    # Run ffx --strict <cmd> with the specified configs, and
    # optionally with a target
    def _run_strict_ffx_with_configs(
        self, cmd: List[str], configs: List[str], target: Optional[str]
    ) -> Any:
        all_args = [
            "--strict",
            "--machine",
            "json",
            "-o",
            "/dev/null",
            *configs,
        ]
        if target is not None:
            all_args += ["-t", target]
        all_args += cmd
        return json.loads(self.run_ffx(all_args))

    # Run ffx --strict <cmd> with the default configs, and
    # optionally with a target
    def _run_strict_ffx(
        self, cmd: List[str], target: Optional[str] = None
    ) -> Any:
        return self._run_strict_ffx_with_configs(
            cmd, self._build_strict_config_args([]), target
        )

    def test_target_echo_no_start_daemon(self) -> None:
        """Test `ffx --strict target echo` does not affect daemon state."""
        output = self._run_strict_ffx(
            [
                "target",
                "echo",
                "From a Test",
            ],
            f"{self.dut_ssh_address}",
        )

        asserts.assert_equal(output, {"message": "From a Test"})
        with asserts.assert_raises(FfxCommandError):
            self.dut.ffx.run(["-c", "daemon.autostart=false", "daemon", "echo"])

    def test_strict_errors_with_target_name(self) -> None:
        """Test `ffx --strict target echo` fails when attempt discovery."""
        with asserts.assert_raises(subprocess.CalledProcessError):
            self._run_strict_ffx(
                [
                    "target",
                    "echo",
                    "From a Test",
                ],
                self.dut_name,
            )

    def test_strict_can_check_for_no_target(self) -> None:
        """Test `ffx --strict target echo` requires a target."""
        with asserts.assert_raises(subprocess.CalledProcessError):
            try:
                self._run_strict_ffx(["target", "echo"], None)
            except subprocess.CalledProcessError as e:
                asserts.assert_true(
                    b"ffx strict requires that the target be explicitly specified"
                    in e.stderr,
                    "The command should require a target",
                )
                raise

    def test_strict_can_accept_no_target(self) -> None:
        """Test `ffx --strict product download` doesn't require a target."""
        with asserts.assert_raises(subprocess.CalledProcessError):
            try:
                self._run_strict_ffx(
                    ["product", "download", "http://0.0.0.0:12345", "foo"], None
                )
            except subprocess.CalledProcessError as e:
                asserts.assert_false(
                    b"ffx strict requires that the target be explicitly specified"
                    in e.stderr,
                    "The command should not require a target",
                )
                raise

    def test_target_list_strict(self) -> None:
        """Test `ffx --strict target list` does not affect daemon state."""
        emu_config = self._get_configs(["emu.instance_dir"])
        configs = self._build_strict_config_args(emu_config)
        output = self._run_strict_ffx_with_configs(
            [
                "target",
                "list",
                self.dut_name,
            ],
            configs,
            None,
        )
        asserts.assert_equal(output[0]["rcs_state"], "Y")
        with asserts.assert_raises(FfxCommandError):
            self.dut.ffx.run(["-c", "daemon.autostart=false", "daemon", "echo"])

    def test_target_list_strict_fails(self) -> None:
        """Test `ffx --strict target list` correctly reports RCS=N."""
        emu_config = self._get_configs(["emu.instance_dir"])
        configs = self._build_strict_config_args(emu_config)
        # Ensure that we cannot find the ssh.priv file
        new_configs = []
        for c in configs:
            if c.startswith("ssh.priv="):
                new_configs.append(c + "NONEXISTENT")
            else:
                new_configs.append(c)
        output = self._run_strict_ffx_with_configs(
            [
                "target",
                "list",
                self.dut_name,
            ],
            new_configs,
            None,
        )
        asserts.assert_equal(output[0]["rcs_state"], "N")

    def test_target_wait_strict(self) -> None:
        """Test `ffx --strict target wait`."""
        output = self._run_strict_ffx(
            [
                "target",
                "wait",
            ],
            f"{self.dut_ssh_address}",
        )
        asserts.assert_equal(output, {"ok": {}})

    def test_target_wait_down_strict(self) -> None:
        """Test `ffx --strict target wait --down`."""
        with asserts.assert_raises(subprocess.CalledProcessError):
            try:
                self._run_strict_ffx(
                    [
                        "target",
                        "wait",
                        "--timeout",
                        "1",
                        "--down",
                    ],
                    f"{self.dut_ssh_address}",
                )
            except subprocess.CalledProcessError as e:
                asserts.assert_in(b"Timeout", e.stderr)
                raise


if __name__ == "__main__":
    test_runner.main()
