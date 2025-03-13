#!/usr/bin/env fuchsia-vendored-python
# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os
import subprocess
import sys

from bazel_tools.tools.python.runfiles import runfiles

r = runfiles.Create()
test_location = r.Rlocation(
    "main/build/bazel/host_tests/py_tests/test_with_py_library"
)
test_env = r.EnvVars()

print(
    f"Found test at {test_location}\nEnvironment: {test_env}\ncwd = {os.getcwd()}"
)
subprocess.check_call([sys.executable, test_location], env=test_env)
print("Test run succesfully!")
