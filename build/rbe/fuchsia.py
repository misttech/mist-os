#!/usr/bin/env python3.8
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Fuchsia-specific constants, functions, conventions.

This includes information like:
  * organization of prebuilt tools
  * path conventions
"""

import glob
import os
import platform
import sys
from typing import Iterable, Sequence

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


HOST_PREBUILT_PLATFORM = _prebuilt_platform_subdir()
HOST_PREBUILT_PLATFORM_SUBDIR = HOST_PREBUILT_PLATFORM

# RBE workers are only linux-x64 at this time, so all binaries uploaded
# for remote execution should use this platform.
# This is also used as a subdir component of prebuilt tool paths.
REMOTE_PLATFORM = 'linux-x64'


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
            yield REMOTE_PLATFORM
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
    'prebuilt', 'third_party', 'rust', REMOTE_PLATFORM)
REMOTE_CLANG_SUBDIR = os.path.join(
    'prebuilt', 'third_party', 'clang', REMOTE_PLATFORM)

# On platforms where ELF utils are unavailable, hardcode rustc's shlibs.
REMOTE_RUSTC_SHLIB_GLOBS = [
    os.path.join(REMOTE_RUSTC_SUBDIR, 'lib', d)
    for d in ('librustc_driver-*.so', 'libstd-*.so', 'libLLVM-*-rust-*.so')
]


def remote_clang_runtime_libdirs(root_rel: str,
                                 target_triple: str) -> Sequence[str]:
    """Locate clang runtime libdir from the current directory."""
    pattern = os.path.join(
        root_rel,
        REMOTE_CLANG_SUBDIR,
        'lib',
        'clang',
        '*',  # a clang version number, like '14.0.0' or '17'
        'lib',
        target_triple,
    )
    return glob.glob(pattern)


def remote_clang_libcxx_static(clang_lib_triple: str) -> str:
    """Location of libc++.a"""
    return os.path.join(
        REMOTE_CLANG_SUBDIR, 'lib', clang_lib_triple, 'libc++.a')


def rust_stdlib_dir(target_triple: str) -> str:
    """Location of rustlib standard libraries.

    This depends on where target libdirs live relative to the rustc compiler.
    It is possible that the target libdirs may move to a location separate
    from where the host tools reside.  See https://fxbug.dev/111727.
    """
    return os.path.join(
        REMOTE_RUSTC_SUBDIR, 'lib', 'rustlib', target_triple, 'lib')


def rustc_target_to_sysroot_triple(target: str) -> str:
    if target.startswith('aarch64-') and '-linux' in target:
        return 'aarch64-linux-gnu'
    if target.startswith('x86_64-') and '-linux' in target:
        return 'x86_64-linux-gnu'
    if target.endswith('-fuchsia'):
        return ''
    if target.startswith('wasm32-'):
        return ''
    raise ValueError(f"unhandled case for sysroot target subdir: {target}")


def rustc_target_to_clang_target(target: str) -> str:
    """Maps a rust target triple to a clang target triple."""
    # These mappings were determined by examining the options available
    # in the clang lib dir, and verifying against traces of libraries accessed
    # by local builds.
    if target == 'aarch64-fuchsia' or (target.startswith('aarch64-') and
                                       target.endswith('-fuchsia')):
        return "aarch64-unknown-fuchsia"
    if target == 'aarch64-linux-gnu' or (target.startswith('aarch64-') and
                                         target.endswith('-linux-gnu')):
        return "aarch64-unknown-linux-gnu"
    if target == 'x86_64-fuchsia' or (target.startswith('x86_64-') and
                                      target.endswith('-fuchsia')):
        return "x86_64-unknown-fuchsia"
    if target == 'x86_64-linux-gnu' or (target.startswith('x86_64-') and
                                        target.endswith('-linux-gnu')):
        return "x86_64-unknown-linux-gnu"
    if target == 'wasm32-unknown-unknown':
        return "wasm32-unknown-unknown"

    raise ValueError(
        f"Unhandled case for mapping to clang lib target dir: {target}")


def remote_rustc_to_rust_lld_path(rustc: str) -> str:
    rust_lld = os.path.join(
        os.path.dirname(rustc),
        # remote is only linux-64
        '../lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-lld')
    return os.path.normpath(rust_lld)


# Built lib/{sysroot_triple}/... files
_SYSROOT_LIB_FILES = (
    'libc.so.6',
    'libpthread.so.0',
    'libm.so.6',
    'libmvec.so.1',
    'librt.so.1',
    'libutil.so.1',
)

# Built /usr/lib/{sysroot_triple}/... files
_SYSROOT_USR_LIB_FILES = (
    'libc.so',
    'libc_nonshared.a',
    'libpthread.so',
    'libpthread.a',
    'libpthread_nonshared.a',
    'libm.so',
    'libm.a',
    'libmvec.so',
    'libmvec.a',
    'libmvec_nonshared.a',
    'librt.so',
    'librt.a',
    'libdl.so',
    'libdl.a',
    'libutil.so',
    'libutil.a',
    'Scrt1.o',
    'crt1.o',
    'crti.o',
    'crtn.o',
)


def sysroot_files(sysroot_dir: str, sysroot_triple: str,
                  with_libgcc: bool) -> Iterable[str]:
    """Expanded list of sysroot files under the Fuchsia build output dir.

    Args:
      sysroot_dir: path to the sysroot, relative to the working dir.
      sysroot_triple: platform-specific subdir of sysroot, based on target.
      with_libgcc: if using `-lgcc`, include additions libgcc support files.

    Yields:
      paths to sysroot files needed for remote/sandboxed linking,
      all relative to the current working dir.
    """
    if sysroot_triple:
        if sysroot_triple.startswith('aarch64-linux'):
            yield f'{sysroot_dir}/lib/{sysroot_triple}/ld-linux-aarch64.so.1'
        elif sysroot_triple.startswith('x86_64-linux'):
            yield f'{sysroot_dir}/lib/{sysroot_triple}/ld-linux-x86-64.so.2'

        for f in _SYSROOT_LIB_FILES:
            yield f"{sysroot_dir}/lib/{sysroot_triple}/{f}"
        for f in _SYSROOT_USR_LIB_FILES:
            yield f"{sysroot_dir}/usr/lib/{sysroot_triple}/{f}"

        if with_libgcc:
            yield from [
                f"{sysroot_dir}/usr/lib/gcc/{sysroot_triple}/4.9/libgcc.a",
                f"{sysroot_dir}/usr/lib/gcc/{sysroot_triple}/4.9/libgcc_eh.a",
                # The toolchain probes (stat) for the existence of crtbegin.o
                f"{sysroot_dir}/usr/lib/gcc/{sysroot_triple}/4.9/crtbegin.o",
                # libgcc also needs sysroot libc.a,
                # although this might be coming from -Cdefault-linker-libraries.
                f"{sysroot_dir}/usr/lib/{sysroot_triple}/libc.a",
            ]
    else:
        yield from [
            f'{sysroot_dir}/lib/libc.so',
            f'{sysroot_dir}/lib/libdl.so',
            f'{sysroot_dir}/lib/libm.so',
            f'{sysroot_dir}/lib/libpthread.so',
            f'{sysroot_dir}/lib/librt.so',
            f'{sysroot_dir}/lib/Scrt1.o',
        ]

        # Not every sysroot dir has a libzircon.
        libzircon_so = f"{sysroot_dir}/lib/libzircon.so"
        if os.path.isfile(libzircon_so):
            yield libzircon_so
