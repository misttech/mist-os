#!/usr/bin/env fuchsia-vendored-python
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
from pathlib import Path
from typing import Iterable, Sequence

_SCRIPT_PATH = Path(__file__)
_SCRIPT_BASENAME = _SCRIPT_PATH.name
_SCRIPT_DIR = _SCRIPT_PATH.parent
_SCRIPT_DIR_REL = Path(os.path.relpath(_SCRIPT_DIR, start=os.curdir))

_EXPECTED_ROOT_SUBDIRS = ('boards', 'bundles', 'prebuilt', 'zircon')


def _dir_is_fuchsia_root(path: Path) -> bool:
    # Look for the presence of certain files and dirs.
    # Cannot always expect .git dir at the root, in the case of
    # a copy or unpacking from archive.
    for d in _EXPECTED_ROOT_SUBDIRS:
        if not (path / d).is_dir():
            return False
    return True


def project_root_dir() -> Path:
    """Returns the root location of the source tree.

    This works as expected when this module is loaded from the
    original location in the Fuchsia source tree.
    However, when this script is copied into a zip archive (as is done
    for `python_host_test` (GN)), it may no longer reside somewhere inside
    the Fuchsia source tree after it is unpacked.
    """
    d = _SCRIPT_DIR.absolute()
    while True:
        if d.name.endswith('.pyz'):
            # If this point is reached, we are NOT operating in the original
            # location in the source tree as intended.  Treat this like a test
            # and return a fake value.
            return Path('/FAKE/PROJECT/ROOT/FOR/TESTING')
        elif _dir_is_fuchsia_root(d):
            return d
        next = d.parent
        if next == d:
            raise Exception(
                f'Unable to find project root searching upward from {_SCRIPT_DIR}.'
            )
        d = next


# This script starts/stops reproxy around a command.
REPROXY_WRAP = _SCRIPT_DIR_REL / "fuchsia-reproxy-wrap.sh"

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


def _path_component_is_platform_subdir(dirname: str) -> bool:
    os, sep, arch = dirname.partition('-')
    return sep == '-' and os in {'linux', 'mac'} and arch in {'x64', 'arm64'}


def _remote_executable_components(host_tool: Path) -> Iterable[str]:
    """Change the path component that correpsonds to the platform."""
    for d in host_tool.parts:
        if _path_component_is_platform_subdir(d):
            yield REMOTE_PLATFORM
        else:
            yield d


def remote_executable(host_tool: Path) -> Path:
    """Locate a corresponding tool for remote execution.

    This assumes that 'linux-x64' is the only available remote execution
    platform, and that the tools are organized by platform next to each other.

    Args:
      host_tool (str): path to an executable for the host platform.

    Returns:
      path to the corresponding executable for the remote execution platform,
      which is currently only linux-x64.
    """
    return Path(*list(_remote_executable_components(host_tool)))


RECLIENT_BINDIR = Path(
    'prebuilt', 'proprietary', 'third_party', 'reclient',
    HOST_PREBUILT_PLATFORM_SUBDIR)

REMOTE_RUSTC_SUBDIR = Path('prebuilt', 'third_party', 'rust', REMOTE_PLATFORM)
REMOTE_CLANG_SUBDIR = Path('prebuilt', 'third_party', 'clang', REMOTE_PLATFORM)
REMOTE_GCC_SUBDIR = Path('prebuilt', 'third_party', 'gcc', REMOTE_PLATFORM)

# TODO(http://fxbug.dev/125627): use platform-dependent location
# Until then, this remote fsatrace only works from linux-x64 hosts.
FSATRACE_PATH = Path('prebuilt', 'fsatrace', 'fsatrace')

_CHECK_DETERMINISM_SCRIPT = Path('build', 'tracer', 'output_cacher.py')


def check_determinism_command(exec_root: Path, outputs: Sequence[Path], command: Sequence[Path] = None, label: str = None) -> Sequence[str]:
    """Returns a command that checks for output determinism.

    The check runs locally twice, moving outputs out of the way temporarily,
    and then compares.

    Args:
      exec_root: path to project root (relative or absolute).
      outputs: output files to compare.
      command: the command to execute.
      label: build system identifier for diagnostics.
    """
    return [
        sys.executable,  # same Python interpreter
        '-S',
        str(exec_root / _CHECK_DETERMINISM_SCRIPT),
    ] + ([f'--label={label}'] if label else []) + [
        '--check-repeatability',
        '--outputs',
    ] + [str(p) for p in outputs] + ['--'] + (command or [])


# On platforms where ELF utils are unavailable, hardcode rustc's shlibs.
def remote_rustc_shlibs(root_rel: Path) -> Iterable[Path]:
    for g in ('librustc_driver-*.so', 'libstd-*.so', 'libLLVM-*-rust-*.so'):
        yield from (root_rel / REMOTE_RUSTC_SUBDIR / 'lib').glob(g)


def remote_clang_runtime_libdirs(root_rel: Path,
                                 target_triple: str) -> Sequence[Path]:
    """Locate clang runtime libdir from the current directory."""
    return (root_rel / REMOTE_CLANG_SUBDIR / 'lib' / 'clang').glob(
        os.path.join(
            '*',  # a clang version number, like '14.0.0' or '17'
            'lib',
            target_triple))


def remote_clang_libcxx_static(clang_lib_triple: str) -> str:
    """Location of libc++.a"""
    return REMOTE_CLANG_SUBDIR / 'lib' / clang_lib_triple / 'libc++.a'


def gcc_support_tools(gcc_path: Path) -> Iterable[Path]:
    bindir = gcc_path.parent
    # expect compiler to be named like {x64_64,aarch64}-elf-{g++,gcc}
    try:
        arch, objfmt, tool = gcc_path.name.split('-')
    except ValueError:
        raise ValueError(
            f'Expecting compiler to be named like {{arch}}-{{objfmt}}-[gcc|g++], but got "{gcc_path.name}"'
        )
    target = '-'.join([arch, objfmt])
    install_root = bindir.parent
    yield install_root / target / "bin/as"
    libexec_base = install_root / "libexec/gcc" / target
    libexec_dirs = list(libexec_base.glob("*"))  # dir is a version number
    if not libexec_dirs:
        return

    libexec_dir = Path(libexec_dirs[0])
    parsers = {"gcc": "cc1", "g++": "cc1plus"}
    yield libexec_dir / parsers[tool]
    # yield libexec_dir / "collect2"  # needed only if linking remotely

    # Workaround: gcc builds a COMPILER_PATH to its related tools with
    # non-normalized paths like:
    # ".../gcc/linux-x64/bin/../lib/gcc/x86_64-elf/12.2.1/../../../../x86_64-elf/bin"
    # The problem is that every partial path of the non-normalized path needs
    # to exist, even if nothing in the partial path is actually used.
    # Here we need the "lib/gcc/x86_64-elf/VERSION" path to exist in the
    # remote environment.  One way to achieve this is to pick an arbitrary
    # file in that directory to include as a remote input, and all of its
    # parent directories will be created in the remote environment.
    version = libexec_dir.name
    # need this dir to exist remotely:
    lib_base = install_root / "lib/gcc" / target / version
    yield lib_base / "crtbegin.o"  # arbitrary unused file, just to setup its dir


def rust_stdlib_subdir(target_triple: str) -> str:
    """Location of rustlib standard libraries relative to rust --sysroot.

    This depends on where target libdirs live relative to the rustc compiler.
    It is possible that the target libdirs may move to a location separate
    from where the host tools reside.  See https://fxbug.dev/111727.
    """
    return Path('lib') / 'rustlib' / target_triple / 'lib'


def rustc_target_to_sysroot_triple(target: str) -> str:
    if target.startswith('aarch64-') and '-linux' in target:
        return 'aarch64-linux-gnu'
    if target.startswith('riscv64gc-') and '-linux' in target:
        return 'riscv64-linux-gnu'
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
    if target == 'riscv64gc-fuchsia' or (target.startswith('riscv64gc-') and
                                         target.endswith('-fuchsia')):
        return "riscv64-unknown-fuchsia"
    if target == 'riscv64gc-linux-gnu' or (target.startswith('riscv64gc-') and
                                           target.endswith('-linux-gnu')):
        return "riscv64-unknown-linux-gnu"
    if target == 'x86_64-fuchsia' or (target.startswith('x86_64-') and
                                      target.endswith('-fuchsia')):
        return "x86_64-unknown-fuchsia"
    if target == 'x86_64-linux-gnu' or (target.startswith('x86_64-') and
                                        target.endswith('-linux-gnu')):
        return "x86_64-unknown-linux-gnu"
    if target == 'wasm32-unknown-unknown':
        return "wasm32-unknown-unknown"

    if target == "x86_64-apple-darwin":
        return "x86_64-apple-darwin"

    raise ValueError(
        f"Unhandled case for mapping to clang lib target dir: {target}")


_REMOTE_RUST_LLD_RELPATH = Path(
    '../lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-lld')


def remote_rustc_to_rust_lld_path(rustc: Path) -> str:
    # remote is only linux-64
    rust_lld = rustc.parent / _REMOTE_RUST_LLD_RELPATH
    return rust_lld  # already normalized by Path construction


# Built lib/{sysroot_triple}/... files
_SYSROOT_LIB_FILES = (
    'libc.so.6',
    'libpthread.so.0',
    'libm.so.6',
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


def c_sysroot_files(sysroot_dir: Path, sysroot_triple: str,
                    with_libgcc: bool) -> Iterable[Path]:
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
            yield sysroot_dir / 'lib' / sysroot_triple / 'ld-linux-aarch64.so.1'
        elif sysroot_triple.startswith('riscv64-linux'):
            yield sysroot_dir / 'lib' / sysroot_triple / 'ld-linux-riscv64-lp64d.so.1'
        elif sysroot_triple.startswith('x86_64-linux'):
            yield sysroot_dir / 'lib' / sysroot_triple / 'ld-linux-x86-64.so.2'

        for f in _SYSROOT_LIB_FILES:
            yield sysroot_dir / 'lib' / sysroot_triple / f
        for f in _SYSROOT_USR_LIB_FILES:
            yield sysroot_dir / 'usr/lib' / sysroot_triple / f

        if with_libgcc:
            yield from [
                sysroot_dir / 'usr/lib/gcc' / sysroot_triple / '4.9/libgcc.a',
                sysroot_dir / 'usr/lib/gcc' / sysroot_triple /
                '4.9/libgcc_eh.a',
                # The toolchain probes (stat) for the existence of crtbegin.o
                sysroot_dir / 'usr/lib/gcc' / sysroot_triple / '4.9/crtbegin.o',
                # libgcc also needs sysroot libc.a,
                # although this might be coming from -Cdefault-linker-libraries.
                sysroot_dir / 'usr/lib' / sysroot_triple / 'libc.a',
            ]
    else:
        yield from [
            sysroot_dir / 'lib/libc.so',
            sysroot_dir / 'lib/libdl.so',
            sysroot_dir / 'lib/libm.so',
            sysroot_dir / 'lib/libpthread.so',
            sysroot_dir / 'lib/librt.so',
            sysroot_dir / 'lib/Scrt1.o',
        ]

        # Not every sysroot dir has a libzircon.
        libzircon_so = sysroot_dir / 'lib/libzircon.so'
        if libzircon_so.is_file():
            yield libzircon_so
