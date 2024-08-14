# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Build information used in the bt-host builds."""

# LINT.IfChange(copts)
# Common C++ flags used in bt-host to ensure it builds in downstream (e.g. Pigweed).
# Note Pigweed downstream has its own version of COPTS since this file is not
# copybara'd.
COPTS = [
    "-Wdeprecated-copy",
    "-Wformat-invalid-specifier",
    "-Wswitch-enum",

    # TODO(https://fxbug.dev/345799180) Remove once once code doesn't have unused parameters.
    "-Wno-unused-parameter",
]
# LINT.ThenChange(BUILD.gn:copts)
