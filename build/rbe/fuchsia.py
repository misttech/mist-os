#!/usr/bin/env python3.8
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Fuchsia-specific constants, functions, conventions.

This includes information like:
  * organization of prebuilt tools
  * path conventions
"""

import os
import platform
import sys
from typing import Iterable

_SCRIPT_BASENAME = os.path.basename(__file__)
_SCRIPT_DIR = os.path.dirname(__file__)
_SCRIPT_DIR_REL = os.path.relpath(_SCRIPT_DIR, start=os.curdir)

_EXPECTED_ROOT_SUBDIRS = ('boards', 'bundles', 'prebuilt', 'zircon')


def _dir_is_fuchsia_root(path) -> bool:
    # Look for the presence of certain files and dirs.
    # Cannot always expect .git dir at the root, in the case of
    # a copy or unpacking from archive.
    for d in _EXPECTED_ROOT_SUBDIRS:
        if not os.path.isdir(os.path.join(path, d)):
            return False
    return True


def project_root_dir() -> str:
    """Returns the root location of the source tree.

    This works as expected when this module is loaded from the
    original location in the Fuchsia source tree.
    However, when this script is copied into a zip archive (as is done
    for `python_host_test` (GN)), it may no longer reside somewhere inside
    the Fuchsia source tree after it is unpacked.
    """
    d = os.path.abspath(_SCRIPT_DIR)
    while True:
        if d.endswith('.pyz'):
            # If this point is reached, we are NOT operating in the original
            # location in the source tree as intended.  Treat this like a test
            # and return a fake value.
            return '/FAKE/PROJECT/ROOT/FOR/TESTING'
        elif _dir_is_fuchsia_root(d):
            return d
        next = os.path.dirname(d)
        if next == d:
            raise Exception(
                f'Unable to find project root searching upward from {_SCRIPT_DIR}.'
            )
        d = next


# This script starts/stops reproxy around a command.
REPROXY_WRAP = os.path.join(_SCRIPT_DIR_REL, "fuchsia-reproxy-wrap.sh")

##########################################################################
### Prebuilt tools
##########################################################################


def _prebuilt_platform_subdir() -> str:
    """Naming convention of prebuilt tools."""
    os_name = {'linux': 'linux', 'darwin': 'mac'}[sys.platform]
    arch = {'x86_64': 'x64', 'arm64': 'arm64'}[platform.machine()]
    return f"{os_name}-{arch}"


HOST_PREBUILT_PLATFORM_SUBDIR = _prebuilt_platform_subdir()

# RBE workers are only linux-x64 at this time, so all binaries uploaded
# for remote execution should use this platform.
REMOTE_PLATFORM_SUBDIR = 'linux-x64'


def _path_components(path: str) -> Iterable[str]:
    for d in path.split(os.path.sep):
        yield d


def _path_component_is_platform_subdir(dirname: str) -> bool:
    os, sep, arch = dirname.partition('-')
    return sep == '-' and os in {'linux', 'mac'} and arch in {'x64', 'arm64'}


def _remote_executable_components(host_tool: str) -> Iterable[str]:
    """Change the path component that correpsonds to the platform."""
    for d in _path_components(host_tool):
        if _path_component_is_platform_subdir(d):
            yield REMOTE_PLATFORM_SUBDIR
        else:
            yield d


def remote_executable(host_tool: str) -> str:
    """Locate a corresponding tool for remote execution.

    This assumes that 'linux-x64' is the only available remote execution
    platform, and that the tools are organized by platform next to each other.

    Args:
      host_tool (str): path to an executable for the host platform.

    Returns:
      path to the corresponding executable for the remote execution platform,
      which is currently only linux-x64.
    """
    return os.path.join(*list(_remote_executable_components(host_tool)))


RECLIENT_BINDIR = os.path.join(
    'prebuilt', 'proprietary', 'third_party', 'reclient',
    HOST_PREBUILT_PLATFORM_SUBDIR)

REMOTE_RUSTC_SUBDIR = os.path.join(
    'prebuilt', 'third_party', 'rust', REMOTE_PLATFORM_SUBDIR)
REMOTE_CLANG_SUBDIR = os.path.join(
    'prebuilt', 'third_party', 'clang', REMOTE_PLATFORM_SUBDIR)

# On platforms where ELF utils are unavailable, hardcode rustc's shlibs.
REMOTE_RUSTC_SHLIB_GLOBS = [
    os.path.join(REMOTE_RUSTC_SUBDIR, 'lib', d)
    for d in ('librustc_driver-*.so', 'libstd-*.so', 'libLLVM-*-rust-*.so')
]


def rust_stdlib_dir(target_triple: str) -> str:
    """Location of rustlib standard libraries.

  This depends on where target libdirs live relative to the rustc compiler.
  It is possible that the target libdirs may move to a location separate
  from where the host tools reside.  See https://fxbug.dev/111727.
  """
    return os.path.join(
        REMOTE_RUSTC_SUBDIR, 'lib', 'rustlib', target_triple, 'lib')
