# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

#!/usr/bin/env python3
import os

import ffx

# Let's assume there's a wrapper class, that might be a standard library made
# available for writing ffx-based tests:


# Given such a wrapper, we can use "ffx --strict". We have to specify
# a couple things that were not necessary before:
# * We must provide the `ssh.priv` file, because it will not be read for
#   the environment.
# * We must say where any logs should go.
#
# In return, our test is now almost as simple, but:
# * It does not use the daemon.
# * This version requires the target to be specified. Alternatively, we could
#   discover a default target, but that discovery would be _explicit_ and under
#   the test's control.
# * The only configurations we override are those required by ffx.
# * The location of the log file is explicit (and required).

# Run with:
# FUCHSIA_NODENAME=<target-ip:port> python3 strict.py

ssh_key = f"{os.environ['HOME']}/.ssh/fuchsia_ed25519"
runner = ffx.FfxRunner("my.log", ssh_key)
# runner.discover_target()
runner.set_target(os.environ["FUCHSIA_NODENAME"])
out = runner.target_echo("foo")
assert out == "foo", f"expected:\n\t'foo'\ngot:\n\t'{out}'"
