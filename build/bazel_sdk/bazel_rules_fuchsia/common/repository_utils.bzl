# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Common utilities for repository rules."""

_FUCHSIA_HOST_OS_MAP = {
    "darwin": "mac",
    "macos": "mac",
    "windows": "win",
}

def get_fuchsia_host_os(repo_ctx):
    """Return host os string according to Fuchsia conventions.

    Args:
      repo_ctx: repository context.
    Returns:
      A string describing the current host system (e.g. 'linux', 'mac' or 'win').
    """
    host_os = repo_ctx.os.name.split(" ")[0]
    return _FUCHSIA_HOST_OS_MAP.get(host_os, host_os)

_FUCHSIA_HOST_ARCH_MAP = {
    "x86_64": "x64",
    "amd64": "x64",
    "aarch64": "arm64",
}

def get_fuchsia_host_arch(repo_ctx):
    """Return host architecture string according to Fuchsia conventions.

    Args:
      repo_ctx: repository context.
    Returns:
      A string describing the current host cpu (e.g. 'x64' or 'arm64').
    """
    host_arch = repo_ctx.os.arch
    return _FUCHSIA_HOST_ARCH_MAP.get(host_arch, host_arch)
