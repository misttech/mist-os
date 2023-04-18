#!/usr/bin/env python3.8
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Wraps a Rust compile command for remote execution (reclient, RBE).

Given a Rust compile command, this script
1) identifies all inputs needed to execute the command hermetically (remotely).
2) identifies all outputs to be retrieved from remote execution.
3) composes a `rewrapper` (reclient) command to request remote execution.
   This includes downloading remotely produced artifacts.
4) forwards stdout/stderr back to the local environment

This script was ported over from build/rbe/rustc-remote-wrapper.sh.

For full usage, see `rustc_remote_wrapper.py -h`.
"""

import argparse
import glob
import os
import subprocess
import sys

import cl_utils
import fuchsia
import rustc
import remote_action

from pathlib import Path
from typing import Any, Iterable, Optional, Sequence

_SCRIPT_BASENAME = Path(__file__).name


def msg(text: str):
    print(f'[{_SCRIPT_BASENAME}] {text}')


# string.removeprefix() only appeared in python 3.9
def remove_prefix(text: str, prefix: str) -> str:
    if text.startswith(prefix):
        return text[len(prefix):]
    return text


# This is needed in some places to workaround b/203540556 (reclient).
def remove_dot_slash_prefix(text: str) -> str:
    return remove_prefix(text, './')


# string.removesuffix() only appeared in python 3.9
def remove_suffix(text: str, suffix: str) -> str:
    if text.endswith(suffix):
        return text[:-len(suffix)]
    return text


# Defined for convenient mocking.
def _readlines_from_file(path: Path) -> Sequence[str]:
    with open(path) as f:
        return f.readlines()


# Defined for convenient mocking.
def _make_local_depfile(command: Sequence[str]) -> int:
    return subprocess.call(command)


# Defined for convenient mocking.
def _tool_is_executable(tool: Path) -> bool:
    return os.access(tool, os.X_OK)


# Defined for convenient mocking.
def _libcxx_isfile(libcxx: Path) -> bool:
    return libcxx.is_file()


# Defined for convenient mocking.
def _env_file_exists(path: Path) -> bool:
    return path.exists()


def _flatten_comma_list_to_paths(items: Iterable[str]) -> Iterable[Path]:
    for item in cl_utils.flatten_comma_list(items):
        yield Path(item)


def _main_arg_parser() -> argparse.ArgumentParser:
    """Construct the argument parser, called by main()."""
    parser = argparse.ArgumentParser(
        description="Prepares a Rust compile command for remote execution.",
        argument_default=[],
    )
    remote_action.inherit_main_arg_parser_flags(parser)
    # There are no Rust-specific options yet.
    return parser


_MAIN_ARG_PARSER = _main_arg_parser()


def depfile_inputs_by_line(lines: Iterable[str]) -> Iterable[Path]:
    """Crude parsing to look at the phony deps of a depfile.

    Phony deps look like:

      some_output_file:

    (nothing following the ':')
    """
    for line in lines:
        s_line = line.rstrip()  # remove trailing '\n'
        if s_line.endswith(':'):
            yield Path(remove_suffix(s_line, ':'))


def relativize_paths(paths: Iterable[Path], start: Path) -> Iterable[Path]:
    for p in paths:
        if p.is_absolute():
            yield cl_utils.relpath(p, start=start)
        else:
            yield p  # Paths are already normalized upon construction


def accompany_rlib_with_so(deps: Iterable[Path]) -> Iterable[Path]:
    """Expand list to include .so files.

    Some Rust libraries come with both .rlib and .so (like libstd), however,
    the depfile generator fails to list the .so file in some cases,
    which causes the build to silently fallback to static linking when
    dynamic linking is requested and intended.  This can result in a mismatch
    between local and remote building.
    See https://github.com/rust-lang/rust/issues/90106
    Workaround (https://fxbug.dev/86896): check for existence of .so and include it.

    Yields:
      original sequence, plus potentially additional .so libs.
    """
    for dep in deps:
        yield dep
        if dep.suffix == '.rlib':
            so_file = dep.with_suffix('.so')  # replaces .rlib with .so
            if so_file.is_file():
                yield so_file


class RustRemoteAction(object):

    def __init__(
        self,
        argv: Sequence[str],
        exec_root: Optional[Path] = None,
        working_dir: Optional[Path] = None,
        host_platform: str = None,
    ):
        self._working_dir = (working_dir or Path(os.curdir)).absolute()
        self._exec_root = (exec_root or remote_action.PROJECT_ROOT).absolute()
        self._host_platform = host_platform or fuchsia.HOST_PREBUILT_PLATFORM

        # Propagate --remote-flag=... options to the remote prefix,
        # as if they appeared before '--'.
        # Forwarded rewrapper options with values must be written as '--flag=value',
        # not '--flag value' because argparse doesn't know what unhandled flags
        # expect values.
        main_argv, filtered_command = remote_action.forward_remote_flags(argv)

        # Forward all unknown flags to rewrapper
        # --help here will result in early exit()
        self._main_args, self._main_remote_options = _MAIN_ARG_PARSER.parse_known_args(
            main_argv)

        if not filtered_command:  # there is no command, bail out early
            return

        self._rust_action = rustc.RustAction(command=filtered_command)

        # TODO: check for missing required flags, like --target

        self._cleanup_files = []
        self._prepare_status = None  # 0 means success, like an exit code

    @property
    def verbose(self) -> bool:
        return self._main_args.verbose

    @property
    def dry_run(self) -> bool:
        return self._main_args.dry_run

    @property
    def label(self) -> str:
        return self._main_args.label

    def value_verbose(self, desc: str, value: str) -> str:
        """In verbose mode, print and forward value."""
        if self.verbose:
            msg(f'{desc}: {value}')
        return value

    def yield_verbose(self, desc: str, items: Iterable[Any]) -> Iterable[Any]:
        """In verbose mode, print and forward items.

        Args:
          desc: text description of what is being printed.
          items: stream of any type of object that is str-able.

        Yields:
          items, unchanged
        """
        if self.verbose:
            msg(f'{desc}: {{')
            for item in items:
                print(f'  {item}')  # one item per line
                yield item
            print(f'}}  # {desc}')
        else:
            yield from items

    @property
    def working_dir(self) -> Path:
        return self._working_dir

    @property
    def exec_root(self) -> Path:
        return self._exec_root

    @property
    def host_platform(self) -> str:
        return self._host_platform

    @property
    def exec_root_rel(self) -> Path:
        # relpath can handle cases that Path.relative_to() cannot.
        return cl_utils.relpath(self.exec_root, start=self.working_dir)

    @property
    def crate_type(self) -> rustc.CrateType:
        return self._rust_action.crate_type

    @property
    def target(self) -> Optional[str]:
        return self._rust_action.target

    @property
    def sysroot(self) -> Optional[Path]:
        return self._rust_action.sysroot

    @property
    def linker(self) -> Optional[Path]:
        return self._rust_action.linker

    @property
    def depfile(self) -> Optional[Path]:
        return self._rust_action.depfile

    @property
    def host_compiler(self) -> Path:
        return self._rust_action.compiler

    @property
    def remote_compiler(self) -> Path:
        return fuchsia.remote_executable(self.host_compiler)

    def remote_compile_command(self) -> Iterable[str]:
        for tok in self._rust_action.command:
            yield str(self.remote_compiler) if tok == str(
                self.host_compiler) else tok

    def _cleanup(self):
        for f in self._cleanup_files:
            f.unlink(missing_ok=True)  # does remove or rmdir

    def _local_depfile_inputs(self) -> Iterable[Path]:
        # Generate a local depfile for the purposes of discovering
        # all transitive inputs.
        local_depfile = Path(str(self.depfile) + '.nolink')
        self._cleanup_files.append(local_depfile)

        dep_only_command = cl_utils.auto_env_prefix_command(
            list(self._rust_action.dep_only_command(local_depfile)))
        dep_status = _make_local_depfile(dep_only_command)
        if dep_status != 0:
            self._prepare_status = dep_status
            return

        remote_depfile_inputs = list(
            depfile_inputs_by_line(_readlines_from_file(local_depfile)))
        # TODO: if needed, transform the rust std lib paths, depending on
        #   Fuchsia directory layout of Rust prebuilt libs.
        #   See remap_remote_rust_lib() in rustc-remote-wrapper.sh
        #   for one possible implementation.

        yield from self.yield_verbose(
            'depfile inputs',
            relativize_paths(
                accompany_rlib_with_so(remote_depfile_inputs),
                self.working_dir))

    def _remote_compiler_inputs(self) -> Iterable[Path]:
        # remote compiler is an input
        remote_compiler = self.remote_compiler
        if not _tool_is_executable(remote_compiler):
            raise Exception(
                f"Remote compilation requires {remote_compiler} to be available for uploading, but it is missing."
            )
        yield self.value_verbose('remote compiler', remote_compiler)
        yield from self._remote_rustc_shlibs()

    def _remote_rustc_shlibs(self) -> Iterable[Path]:
        """Find remote compiler shared libraries.

        Yields:
          shared library paths relative to current working dir.
        """
        if self.host_platform == fuchsia.REMOTE_PLATFORM:
            # remote and host execution environments match
            yield from self.yield_verbose(
                'remote compiler shlibs (detected)',
                relativize_paths(
                    remote_action.host_tool_nonsystem_shlibs(
                        self.remote_compiler), self.working_dir))
        else:
            yield from self._assumed_remote_rustc_shlibs()

    def _assumed_remote_rustc_shlibs(self) -> Iterable[Path]:
        # KLUDGE: the host's binutils may not be able to analyze the remote
        # executable (ELF), so hardcode what we think the shlibs are.
        yield from self.yield_verbose(
            'remote compiler shlibs (guessed)',
            fuchsia.remote_rustc_shlibs(self.exec_root_rel))

    def _rust_stdlib_libunwind_inputs(self) -> Iterable[Path]:
        # The majority of stdlibs already appear in dep-info and are uploaded
        # as needed.  However, libunwind.a is not listed, but is directly
        # needed by code emitted by rustc.  Listing this here works around a
        # missing upload issue, and adheres to the guidance of listing files
        # instead of whole directories.
        if not self.target:
            return
        libunwind_a = Path(
            self.exec_root_rel, fuchsia.rust_stdlib_dir(self.target),
            'libunwind.a')
        if libunwind_a.exists():
            yield self.value_verbose('libunwind', libunwind_a)

    def _inputs_from_env(self) -> Iterable[Path]:
        """Scan command environment variables for references to inputs files.

        If a variable value looks like a path and it points to something
        that exists, assume it is needed for remote execution.
        """
        for e in self._rust_action.env:
            key, sep, value = e.partition('=')
            if sep == '=':
                try:
                    p = Path(value)
                    if _env_file_exists(p):
                        yield cl_utils.relpath(p, start=self.working_dir)
                except ValueError:  # value is not a Path, ignore it
                    pass

    def _remote_inputs(self) -> Iterable[Path]:
        """Remote inputs are relative to current working dir."""
        yield self.value_verbose(
            'top source', self._rust_action.direct_sources[0])
        yield from self._local_depfile_inputs()

        yield from self._remote_compiler_inputs()

        yield from self._rust_stdlib_libunwind_inputs()

        # Indirect dependencies (libraries)
        yield from self.yield_verbose(
            'extern libs', self._rust_action.extern_paths())

        # Prefer to have the build system's command specify additional
        # --remote-inputs instead of trying to infer them.
        # yield from self.yield_verbose('env var files', self._inputs_from_env())

        # Link arguments like static libs are checked for existence
        # but not necessarily used until linking a binary.
        # This is why we need to includes link_arg_files unconditionally,
        # whereas the linker tools themselves can be dropped until linking binaries.
        yield from self.yield_verbose(
            'link arg files', self._rust_action.link_arg_files)

        if self._rust_action.needs_linker:
            yield from self._remote_linker_inputs()

        yield from self.yield_verbose(
            'forwarded inputs',
            _flatten_comma_list_to_paths(self._main_args.inputs))

    def _remote_output_files(self) -> Iterable[Path]:
        """Remote output files are relative to current working dir."""
        yield self.value_verbose('main output', self._rust_action.output_file)

        depfile = self.depfile
        if depfile:
            yield self.value_verbose('depfile', depfile)

        yield from self.yield_verbose(
            'extra compiler outputs', self._rust_action.extra_output_files())

        yield from self.yield_verbose(
            'forwarded output files',
            _flatten_comma_list_to_paths(self._main_args.output_files))

    def _remote_output_dirs(self) -> Iterable[Path]:
        yield from self.yield_verbose(
            'forwarded output dirs',
            _flatten_comma_list_to_paths(self._main_args.output_directories))

    @property
    def remote_options(self) -> Sequence[str]:
        """rewrapper remote execution options."""
        fixed_remote_options = [
            # type=tool says we are providing a custom tool (Rust compiler), and
            #   thus, own the logic for providing explicit inputs.
            # shallow=true works around an issue where racing mode downloads
            #   incorrectly
            "--labels=type=tool,shallow=true",
            # --canonicalize_working_dir: coerce the output dir to a constant.
            #   This requires that the command be insensitive to output dir, and
            #   that its outputs do not leak the remote output dir.
            #   Ensuring that the results reproduce consistently across different
            #   build directories helps with caching.
            "--canonicalize_working_dir=true",
        ]

        # _main_remote_options should be allowed to override the
        # fixed_remote_options
        return fixed_remote_options + self._main_remote_options

    def prepare(self) -> int:
        """Setup the remote action, but do not execute it.

        Returns:
          exit code, 0 for success
        """
        # cache the preparation
        if self._prepare_status is not None:
            return self._prepare_status

        # inputs and outputs are relative to current working dir
        remote_inputs = list(self._remote_inputs())
        remote_output_files = list(self._remote_output_files())
        remote_output_dirs = list(self._remote_output_dirs())

        self._remote_action = remote_action.remote_action_from_args(
            main_args=self._main_args,
            remote_options=self.remote_options,
            command=list(self.remote_compile_command()),
            inputs=remote_inputs,
            output_files=remote_output_files,
            output_dirs=remote_output_dirs,
            working_dir=self.working_dir,
            exec_root=self.exec_root,
        )
        self._prepare_status = 0  # exit code success
        return self._prepare_status

    @property
    def remote_action(self) -> remote_action.RemoteAction:
        return self._remote_action

    def _remote_linker_executables(self) -> Iterable[Path]:
        if self.linker:
            remote_linker = fuchsia.remote_executable(self.linker)
            yield self.value_verbose('remote linker', remote_linker)

            # ld.lld -> lld, but the symlink is required for the clang
            # linker driver to be able to use lld.
            yield self.value_verbose(
                'remote rust lld', remote_linker.parent / 'ld.lld')

    def _remote_libcxx(self, clang_lib_triple: str) -> Iterable[Path]:
        libcxx_remote = self.exec_root_rel / fuchsia.remote_clang_libcxx_static(
            clang_lib_triple)
        if _libcxx_isfile(libcxx_remote):
            yield self.value_verbose('remote libc++', libcxx_remote)

    def _remote_clang_runtime_libs(self,
                                   clang_lib_triple: str) -> Iterable[Path]:
        # clang runtime lib dir
        rt_libdir_remote = list(
            fuchsia.remote_clang_runtime_libdirs(
                self.exec_root_rel, clang_lib_triple))
        # if none found, that's ok.
        if len(rt_libdir_remote) == 1:
            yield self.value_verbose(
                'remote runtime libdir', rt_libdir_remote[0])
        if len(rt_libdir_remote) > 1:
            raise Exception(
                f"Found more than one clang runtime lib dir (don't know which one to use): {rt_libdir_remote}"
            )

    def _sysroot_files(self) -> Iterable[Path]:
        # sysroot files
        if not self.target:
            return
        sysroot_dir = self.sysroot
        sysroot_triple = fuchsia.rustc_target_to_sysroot_triple(self.target)
        if sysroot_dir:
            yield from self.yield_verbose(
                'sysroot files',
                fuchsia.sysroot_files(
                    sysroot_dir=sysroot_dir,
                    sysroot_triple=sysroot_triple,
                    with_libgcc=self._rust_action.want_sysroot_libgcc,
                ))

    def _remote_linker_inputs(self) -> Iterable[Path]:
        if self.linker:
            yield from self._remote_linker_executables()

            if self.target:
                clang_lib_triple = fuchsia.rustc_target_to_clang_target(
                    self.target)
                yield from self._remote_libcxx(clang_lib_triple)
                yield from self._remote_clang_runtime_libs(clang_lib_triple)

        # --crate-type cdylib needs rust-lld (hard-coding this is a hack)
        # This will always be linux-x64, even when cross-compiling, because
        # that is the only RBE remote backend option available.
        if self.crate_type == rustc.CrateType.CDYLIB:
            yield self.value_verbose(
                'remote rust lld',
                fuchsia.remote_rustc_to_rust_lld_path(self.remote_compiler))

        yield from self._sysroot_files()

    def _depfile_exists(self) -> bool:
        # Defined for easy mocking.
        return self.depfile and self.depfile.exists()

    def _rewrite_depfile(self):
        """Rewrite depfile without working dir absolute paths.

        TEMPORARY WORKAROUND until upstream fix lands:
          https://github.com/pest-parser/pest/pull/522
        Rewrite the depfile if it contains any absolute paths from the remote
        build; paths should be relative to the root_build_dir.

        Assume that the output dir is two levels down from the exec_root.

        When using the `canonicalize_working_dir` rewrapper option,
        the output directory is coerced to a predictable 'set_by_reclient' constant.
        See https://source.corp.google.com/foundry-x-re-client/internal/pkg/reproxy/action.go;l=131
        It is still possible for a tool to leak absolute paths, which could
        expose that constant in returned artifacts.
        We forgive this for depfiles, but other artifacts should be verified
        separately.
        """
        remote_action.remove_working_dir_abspaths_from_depfile_in_place(
            self.depfile, self.remote_action.build_subdir)

    def run(self) -> int:
        prepare_status = self.prepare()
        if prepare_status != 0:
            return prepare_status

        try:
            return self.remote_action.run_with_main_args(self._main_args)

        finally:
            if self._depfile_exists():
                self._rewrite_depfile()
            self._cleanup()


def main(argv: Sequence[str]) -> int:
    rust_remote_action = RustRemoteAction(
        argv,  # [remote options] -- rustc-compile-command...
        exec_root=remote_action.PROJECT_ROOT,
        working_dir=Path(os.curdir),
        host_platform=fuchsia.HOST_PREBUILT_PLATFORM,
    )
    return rust_remote_action.run()


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
