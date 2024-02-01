#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Entry point to Mypy type checker command line tool."""

import sys
from mypy import api

def main():
    try:
        stdout, stderr, exit_status = api.run(sys.argv[1:])
        if exit_status != 0:
            if stdout:
                print(stdout)
            if stderr:
                print(stderr)
        return 0
    except Exception as e: # [broad-exception-caught]
        print(f"Error occurred during Mypy execution: {e}")
        return 1
