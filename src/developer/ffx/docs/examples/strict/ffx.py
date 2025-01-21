# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

#!/usr/bin/env python3
import json
import subprocess
import sys

# Sample wrapper class, good enough to support the strict.py example.


class FfxRunner:
    def __init__(self, log_file, ssh_key, verbose=False):
        self.log_file = log_file
        self.ssh_key = ssh_key
        self.verbose = verbose
        self.target = None

    def debug(self, message):
        if self.verbose:
            print(message, file=sys.stderr)

    def set_target(self, target):
        self.target = target

    def run_raw(self, opts, args):
        cmd = ["ffx"]
        cmdline = cmd + opts + args
        self.debug(" ".join(cmdline))
        try:
            return subprocess.run(
                cmdline, check=True, capture_output=True
            ).stdout
        except subprocess.CalledProcessError as e:
            print(e.stderr.decode("utf-8"))
            raise

    def run(self, target, opts, args):
        if target:
            opts.extend(["--target", target])
        out = self.run_raw(opts + ["--machine", "json"], args)
        return json.loads(out)

    def required_configs(self):
        return [f"ssh.priv={self.ssh_key}"]

    def run_strict(self, target, args, extra_configs=[]):
        cfgs = ",".join(self.required_configs() + extra_configs)
        res = self.run(
            target, ["--strict", "--config", cfgs, "-o", self.log_file], args
        )
        self.debug(res)
        if "unexpected_error" in res:
            raise Exception(res["unexpected_error"])
        return res

    def discover_target(self):
        targets = self.run(None, [], ["target", "list"])
        if len(targets) != 1:
            raise Exception("cannot determine default target")
        addr = targets[0]["addresses"][0]
        self.target = f"{addr['ip']}:{addr['ssh_port']}"

    def target_echo(self, msg):
        res = self.run_strict(self.target, ["target", "echo", msg])
        return res["message"]

    def target_list(self, emu_instance_dir):
        if emu_instance_dir:
            extra_configs = [f"emu.instance_dir={emu_instance_dir}"]
        else:
            extra_configs = ["emu.instance_dir="]
        res = self.run_strict(self.target, ["target", "list"], extra_configs)
        return res

    def target_show(self):
        res = self.run_strict(self.target, ["target", "show"])
        return res

    def component_list(self):
        res = self.run_strict(self.target, ["component", "list"])
        return res
