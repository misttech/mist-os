#!/usr/bin/env fuchsia-vendored-python

# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Helper functions for executing SDK Developer workflow tests.
"""

import subprocess
from typing import List


class _SDKCommand:
    def __init__(self, cmd: str):
        self.command = cmd
        self.should_capture_output = False
        self.check = True
        self.kwargs = {
            "check": True,
            "stdout": subprocess.PIPE,
            "stderr": subprocess.STDOUT,
        }

    def cwd(self, cwd: str):
        self.kwargs["cwd"] = cwd
        return self

    def capture(self):
        self.should_capture_output = True
        return self

    def captured(self) -> str:
        return self.stdout.strip()

    def execute(self):
        print("---")
        print("-> {}".format(self.command))
        if "cwd" in self.kwargs:
            print("-> (cwd = {})".format(self.kwargs["cwd"]))
        try:
            p = subprocess.run(
                self.command.split(),
                **{k: v for k, v in self.kwargs.items() if v is not None},
            )
            self.stdout = p.stdout.decode().strip()
            [print(l) for l in self.stdout.splitlines()]
        except Exception as e:
            self.stdout = str(e)
            raise e


class SDKCommands:
    def __init__(self):
        self.command_list = []

    def _append(self, c: str):
        cmd = _SDKCommand(c)
        self.command_list.append(cmd)
        return cmd

    def captured(self) -> str:
        return "".join(
            [c.captured() for c in self.command_list if c.should_capture_output]
        )

    def bootstrap(self):
        return self._append("scripts/bootstrap.sh")

    def ffx(self, cmd: str):
        return self._append("{} {}".format("ffx", cmd))

    def run(self, cmd: str):
        return self._append(cmd)

    def execute(self) -> None:
        print("Commands:\n")
        [c.execute() for c in self.command_list]
