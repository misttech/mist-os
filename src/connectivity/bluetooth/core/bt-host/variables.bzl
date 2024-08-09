# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Build information used in the bt-host builds."""

# Common copts used in bt-host to ensure it builds in downstream (e.g. Pigweed)
# LINT.IfChange(copts)
COPTS = [
    "-Wswitch-enum",
]
# LINT.ThenChange(BUILD.gn:copts)
