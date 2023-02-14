# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Fuchsia related platforms utility functions."""

def to_bazel_cpu_name(fuchsia_cpu):
    """Convert Fuchsia CPU architecture name to Bazel compatible one."""
    if fuchsia_cpu == "x64":
        return "x86_64"
    else:
        return fuchsia_cpu

def to_fuchsia_cpu_name(bazel_cpu):
    """Convert Bazel cpu name to FUchsia cpu architecture name."""
    if bazel_cpu in ("amd64", "k8", "x86_64"):
        return "x64"
    else:
        return bazel_cpu
