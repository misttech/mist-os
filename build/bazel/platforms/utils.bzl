# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Fuchsia related platforms utility functions."""

def to_bazel_cpu_name(fuchsia_cpu):
    """Convert Fuchsia CPU architecture name to Bazel compatible one.

    Args:
        fuchsia_cpu: A Fuchsia-compatible cpu name (e.g. "x64").
    Returns:
        A Bazel compatible cpu name, e.g. "x86_64", so that
        @platforms//cpu:<result> is a valid label.
    """
    if fuchsia_cpu == "x64":
        return "x86_64"
    else:
        return fuchsia_cpu

def to_fuchsia_cpu_name(bazel_cpu):
    """Convert Bazel cpu name to FUchsia cpu architecture name.

    Args:
        bazel_cpu: A Bazel-compatible cpu name, so that
        @platforms//cpu:<bazel_cpu> is a valid label,
    Returns:
        The corresponding Fuchsia cpu name (e.g. 'x64')
    """
    if bazel_cpu in ("amd64", "k8", "x86_64"):
        return "x64"
    else:
        return bazel_cpu

def to_bazel_os_name(fuchsia_os):
    """Convert Fuchsia OS name to Bazel compatible one.

    Args:
        fuchsia_os: A Fuchsia-compatible operating system name (e.g. "mac")
    Returns:
        A Bazel compatible os name, e.g. "osx", so that
        @platforms//os:<result> is a valid label.
    """
    if fuchsia_os == "mac":
        return "osx"
    elif fuchsia_os == "win":
        return "windows"
    else:
        return fuchsia_os

def to_fuchsia_os_name(bazel_os):
    """Convert Bazel OS name to FUchsia cpu compatibel one.

    Args:
        bazel_os: A Bazel compatible operating system name, so that
            @platforms//os:<bazel_os> is a valid label.
    Returns:
        A Fuchsia compatible operating system name string (e.g. "mac")
    """
    if bazel_os in ("macos", "osx"):
        # @platforms//os:macos and @platforms//os:osx are equivalent.
        return "mac"
    elif bazel_os == "windows":
        return "win"
    else:
        return bazel_os
