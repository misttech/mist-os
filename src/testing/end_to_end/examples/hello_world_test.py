#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Simple Hello World version of a Fuchsia Mobly test."""

from mobly import base_test
from mobly import test_runner
import fuchsia_device


class HelloWorldTest(base_test.BaseTestClass):

    def setup_class(self):
        # Use Mobly's built-in register_controller() to parse configs and create
        # devices controllers for test usage.
        fuchsia_devices = self.register_controller(fuchsia_device)
        self.dut = fuchsia_devices[0]

    def test_say_hello(self):
        self.dut.hello()

    def test_print_params(self):
        for k, v in self.user_params.items():
            print(f'{k}: {v}')
