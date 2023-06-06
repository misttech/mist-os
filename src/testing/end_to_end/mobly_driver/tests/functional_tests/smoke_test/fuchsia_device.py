#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Simple Fuchsia Mobly controller.

This implements the required APIs to make this module compatible with Mobly.
 - create()
 - destroy()
 - MOBLY_CONTROLLER_CONFIG_NAME
"""

MOBLY_CONTROLLER_CONFIG_NAME = 'FuchsiaDevice'
CONFIG_KEY_NAME = 'name'


def create(configs):
    """Parses config YAML and create device controller(s)."""
    fds = []
    for config in configs:
        fds.append(FuchsiaDevice(config))
    return fds


def destroy(unused_devices):
    """Tears down the controller(s)."""


class FuchsiaDevice():
    """Trivial Fuchsia device controller class."""

    def __init__(self, config):
        self._name = config[CONFIG_KEY_NAME]
