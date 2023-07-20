#!/usr/bin/env fuchsia-vendored-python
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Tests for rustc_remote_wrapper."""

import contextlib
import io
import os
import subprocess
import sys
import tempfile
import unittest

from pathlib import Path
from unittest import mock

import rustc_remote_wrapper
import rustc
import cl_utils
import fuchsia
import remote_action

from typing import Any, Iterable, Sequence


class ImmediateExit(Exception):
    """For mocking functions that do not return."""
    pass


def _write_file_contents(path: Path, contents: str):
    with open(path, 'w') as f:
        f.write(contents)


def _read_file_contents(path: Path) -> str:
    with open(path, 'r') as f:
        return f.read()


def _strs(items: Sequence[Any]) -> Sequence[str]:
    return [str(i) for i in items]


def _paths(items: Sequence[Any]) -> Sequence[Path]:
    if isinstance(items, list):
        return [Path(i) for i in items]
    elif isinstance(items, set):
        return {Path(i) for i in items}
    elif isinstance(items, tuple):
        return tuple(Path(i) for i in items)

    t = type(items)
    raise TypeError(f"Unhandled sequence type: {t}")


class TestAccompanyRlibWithSo(unittest.TestCase):

    def test_not_rlib(self):
        deps = Path('../path/to/foo.rs')
        self.assertEqual(
            list(rustc_remote_wrapper.accompany_rlib_with_so([deps])), [deps])

    def test_rlib_without_so(self):
        deps = Path('obj/build/foo.rlib')
        with mock.patch.object(Path, 'is_file',
                               return_value=False) as mock_isfile:
            self.assertEqual(
                list(rustc_remote_wrapper.accompany_rlib_with_so([deps])),
                [deps])
        mock_isfile.assert_called_once()

    def test_rlib_with_so(self):
        deps = Path('obj/build/foo.rlib')
        with mock.patch.object(Path, 'is_file',
                               return_value=True) as mock_isfile:
            self.assertEqual(
                list(rustc_remote_wrapper.accompany_rlib_with_so([deps])),
                [deps, Path('obj/build/foo.so')])
            mock_isfile.assert_called_once()


class RustRemoteActionPrepareTests(unittest.TestCase):

    def generate_prepare_mocks(
        self,
        depfile_contents: Sequence[str] = None,
        compiler_shlibs: Sequence[Path] = None,
        clang_rt_libdir: Path = None,
        libcxx_static: Path = None,
    ) -> Iterable[mock.patch.object]:
        """common mocks needed for RustRemoteAction.prepare()"""
        # depfile only command
        yield mock.patch.object(
            rustc_remote_wrapper, '_make_local_depfile', return_value=0)

        if depfile_contents:
            yield mock.patch.object(
                rustc_remote_wrapper,
                '_readlines_from_file',
                return_value=[line + '\n' for line in depfile_contents])

        # check compiler is executable
        yield mock.patch.object(
            rustc_remote_wrapper, '_tool_is_executable', return_value=True)

        if compiler_shlibs:
            yield mock.patch.object(
                rustc_remote_wrapper.RustRemoteAction,
                '_remote_rustc_shlibs',
                return_value=iter(compiler_shlibs))

        yield mock.patch.object(
            rustc_remote_wrapper, '_libcxx_isfile', return_value=True)

        if clang_rt_libdir:
            yield mock.patch.object(
                fuchsia,
                'remote_clang_runtime_libdirs',
                return_value=[clang_rt_libdir])

        if libcxx_static:
            yield mock.patch.object(
                fuchsia,
                'remote_clang_libcxx_static',
                return_value=libcxx_static or '')

        # expected to be called through _rust_stdlib_libunwind_inputs()
        # when --sysroot is unspecified.
        yield mock.patch.object(
            rustc.RustAction,
            'default_rust_sysroot',
            return_value=Path('../some/random/sysroot'))

    def test_prepare_basic(self):
        exec_root = Path('/home/project')
        working_dir = exec_root / 'build-here'
        compiler = Path('../tools/bin/rustc')
        shlib = Path('tools/lib/librusteze.so')
        shlib_abs = exec_root / shlib
        shlib_rel = cl_utils.relpath(shlib_abs, start=working_dir)
        source = Path('../foo/src/lib.rs')
        rlib = Path('obj/foo.rlib')
        deps = [Path('../foo/src/other.rs')]
        depfile_contents = [str(d) + ':' for d in deps]
        command = _strs([compiler, source, '-o', rlib])
        r = rustc_remote_wrapper.RustRemoteAction(
            ['--'] + command,
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )

        mocks = self.generate_prepare_mocks(
            depfile_contents=depfile_contents,
            compiler_shlibs=[shlib_rel],
        )
        with contextlib.ExitStack() as stack:
            for m in mocks:
                stack.enter_context(m)
            prepare_status = r.prepare()

        self.assertEqual(prepare_status, 0)  # success
        a = r.remote_action
        remote_inputs = set(a.inputs_relative_to_working_dir)
        remote_output_files = set(a.output_files_relative_to_working_dir)
        self.assertEqual(
            remote_inputs, set([compiler, shlib_rel, source] + deps))
        self.assertEqual(remote_output_files, {rlib})

    def test_prepare_with_response_file(self):
        exec_root = Path('/home/project')
        working_dir = exec_root / 'build-here'
        compiler = Path('../tools/bin/rustc')
        shlib = Path('tools/lib/librusteze.so')
        shlib_abs = exec_root / shlib
        shlib_rel = cl_utils.relpath(shlib_abs, start=working_dir)
        source = Path('../foo/src/lib.rs')
        rlib = Path('obj/foo.rlib')
        deps = [Path('../foo/src/other.rs')]
        depfile_contents = [str(d) + ':' for d in deps]

        with tempfile.TemporaryDirectory() as td:
            tdp = Path(td)
            rsp = tdp / 'foo.rsp'
            rsp.write_text(
                '\n'.join(_strs(['# expand these args', source, '-o', rlib])) +
                '\n')
            command = _strs(
                [compiler, f'@{rsp}'])  # pass args through response file
            r = rustc_remote_wrapper.RustRemoteAction(
                ['--'] + command,
                exec_root=exec_root,
                working_dir=working_dir,
                auto_reproxy=False,
            )

        mocks = self.generate_prepare_mocks(
            depfile_contents=depfile_contents,
            compiler_shlibs=[shlib_rel],
        )
        with contextlib.ExitStack() as stack:
            for m in mocks:
                stack.enter_context(m)
            prepare_status = r.prepare()

        self.assertEqual(prepare_status, 0)  # success
        a = r.remote_action
        remote_inputs = set(a.inputs_relative_to_working_dir)
        remote_output_files = set(a.output_files_relative_to_working_dir)
        self.assertEqual(
            remote_inputs, set([compiler, shlib_rel, source, rsp] + deps))
        self.assertEqual(remote_output_files, {rlib})

    def test_prepare_depfile(self):
        exec_root = Path('/home/project')
        working_dir = exec_root / 'build-here'
        compiler = Path('../tools/bin/rustc')
        shlib = Path('tools/lib/librusteze.so')
        shlib_abs = exec_root / shlib
        shlib_rel = cl_utils.relpath(shlib_abs, start=working_dir)
        source = Path('../foo/src/lib.rs')
        rlib = Path('obj/foo.rlib')
        deps = [Path('../foo/src/other.rs')]
        depfile_path = Path('obj/foo.rlib.d')
        depfile_contents = [str(d) + ':' for d in deps]
        command = _strs(
            [compiler, source, '-o', rlib, f'--emit=dep-info={depfile_path}'])
        r = rustc_remote_wrapper.RustRemoteAction(
            ['--'] + command,
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )

        mocks = self.generate_prepare_mocks(
            depfile_contents=depfile_contents,
            compiler_shlibs=[shlib_rel],
        )
        with contextlib.ExitStack() as stack:
            for m in mocks:
                stack.enter_context(m)
            prepare_status = r.prepare()

        self.assertEqual(prepare_status, 0)  # success
        a = r.remote_action
        remote_inputs = set(a.inputs_relative_to_working_dir)
        remote_output_files = set(a.output_files_relative_to_working_dir)
        self.assertEqual(
            remote_inputs, set([compiler, shlib_rel, source] + deps))
        self.assertEqual(remote_output_files, {rlib, depfile_path})
        self.assertEqual(set(a.always_download), set([depfile_path]))

    def test_prepare_depfile_failure(self):
        exec_root = Path('/home/project')
        working_dir = exec_root / 'build-here'
        compiler = Path('../tools/bin/rustc')
        source = Path('../foo/src/lib.rs')
        rlib = Path('obj/foo.rlib')
        depfile_path = Path('obj/foo.rlib.d')
        # mocking failure, no need for actual depfile contents
        command = _strs(
            [compiler, source, '-o', rlib, f'--emit=dep-info={depfile_path}'])
        r = rustc_remote_wrapper.RustRemoteAction(
            ['--'] + command,
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )

        # Make sure internal RuntimeError gets handled.
        with mock.patch.object(rustc_remote_wrapper, '_make_local_depfile',
                               return_value=1) as mock_deps:
            prepare_status = r.prepare()

        self.assertEqual(prepare_status, 1)  # failure

    def test_prepare_externs(self):
        exec_root = Path('/home/project')
        working_dir = exec_root / 'build-here'
        compiler = Path('../tools/bin/rustc')
        shlib = Path('tools/lib/librusteze.so')
        shlib_abs = exec_root / shlib
        shlib_rel = cl_utils.relpath(shlib_abs, start=working_dir)
        source = Path('../foo/src/lib.rs')
        rlib = Path('obj/foo.rlib')
        deps = [Path('../foo/src/other.rs')]
        depfile_contents = [str(d) + ':' for d in deps]
        externs = _paths([
            'obj/path/to/this.rlib',
            'obj/path/to/that.rlib',
        ])
        extern_flags = [
            '--extern',
            f'this={externs[0]}',
            '--extern',
            f'that={externs[1]}',
        ]
        command = _strs([compiler, source, '-o', rlib]) + extern_flags
        r = rustc_remote_wrapper.RustRemoteAction(
            ['--'] + command,
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )

        mocks = self.generate_prepare_mocks(
            depfile_contents=depfile_contents,
            compiler_shlibs=[shlib_rel],
        )
        with contextlib.ExitStack() as stack:
            for m in mocks:
                stack.enter_context(m)
            prepare_status = r.prepare()

        self.assertEqual(prepare_status, 0)  # success
        a = r.remote_action
        remote_inputs = set(a.inputs_relative_to_working_dir)
        remote_output_files = set(a.output_files_relative_to_working_dir)
        self.assertEqual(
            remote_inputs, set([compiler, shlib_rel, source] + deps + externs))
        self.assertEqual(remote_output_files, {rlib})

    def test_prepare_link_arg_files(self):
        exec_root = Path('/home/project')
        working_dir = exec_root / 'build-here'
        compiler = Path('../tools/bin/rustc')
        shlib = Path('tools/lib/librusteze.so')
        shlib_abs = exec_root / shlib
        shlib_rel = cl_utils.relpath(shlib_abs, start=working_dir)
        source = Path('../foo/src/lib.rs')
        rlib = Path('obj/foo.rlib')
        deps = [Path('../foo/src/other.rs')]
        depfile_contents = [str(d) + ':' for d in deps]
        link_arg = Path('obj/some/random.a')
        command = _strs(
            [compiler, source, '-o', rlib, f'-Clink-arg={link_arg}'])
        r = rustc_remote_wrapper.RustRemoteAction(
            ['--'] + command,
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )

        mocks = self.generate_prepare_mocks(
            depfile_contents=depfile_contents,
            compiler_shlibs=[shlib_rel],
        )
        with contextlib.ExitStack() as stack:
            for m in mocks:
                stack.enter_context(m)
            prepare_status = r.prepare()

        self.assertEqual(prepare_status, 0)  # success
        a = r.remote_action
        remote_inputs = set(a.inputs_relative_to_working_dir)
        remote_output_files = set(a.output_files_relative_to_working_dir)
        self.assertEqual(
            remote_inputs,
            set([compiler, shlib_rel, source] + deps + [link_arg]))
        self.assertEqual(remote_output_files, {rlib})

    def test_prepare_needs_linker(self):
        exec_root = Path('/home/project')
        working_dir = exec_root / 'build-here'
        exec_root_rel = cl_utils.relpath(exec_root, start=working_dir)
        compiler = Path('../tools/bin/rustc')
        shlib = Path('tools/lib/librusteze.so')
        shlib_abs = exec_root / shlib
        shlib_rel = cl_utils.relpath(shlib_abs, start=working_dir)
        linker = Path('../tools/bin/linker')
        lld = Path('../tools/bin/ld.lld')
        clang_rt_libdir = Path('../fake/lib/clang/x86_64-unknown-linux')
        libcxx = Path('fake/clang/libc++.a')
        source = Path('../foo/src/lib.rs')
        target = 'x86_64-unknown-linux-gnu'
        exe = Path('obj/foo.exe')
        deps = [Path('../foo/src/other.rs')]
        depfile_contents = [str(d) + ':' for d in deps]
        command = _strs(
            [
                compiler, source, '-o', exe, '--crate-type', 'bin',
                f'-Clinker={linker}', f'--target={target}',
                f'-Clink-arg=-fuse-ld=lld'
            ])
        r = rustc_remote_wrapper.RustRemoteAction(
            ['--'] + command,
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )
        self.assertEqual(r.linker, linker)
        self.assertEqual(r.remote_ld_path, lld)

        mocks = self.generate_prepare_mocks(
            depfile_contents=depfile_contents,
            compiler_shlibs=[shlib_rel],
            clang_rt_libdir=clang_rt_libdir,
            libcxx_static=libcxx,
        )
        with contextlib.ExitStack() as stack:
            for m in mocks:
                stack.enter_context(m)
            prepare_status = r.prepare()

        self.assertEqual(prepare_status, 0)  # success
        a = r.remote_action
        remote_inputs = set(a.inputs_relative_to_working_dir)
        remote_output_files = set(a.output_files_relative_to_working_dir)
        self.assertEqual(
            remote_inputs,
            set(
                _paths(
                    [compiler, shlib_rel, source] + deps +
                    [linker, lld, clang_rt_libdir, exec_root_rel / libcxx])))
        self.assertEqual(remote_output_files, {exe})

    def test_prepare_remote_option_forwarding(self):
        exec_root = Path('/home/project')
        working_dir = exec_root / 'build-here'
        compiler = Path('../tools/bin/rustc')
        shlib = Path('tools/lib/librusteze.so')
        shlib_abs = exec_root / shlib
        shlib_rel = cl_utils.relpath(shlib_abs, start=working_dir)
        source = Path('../foo/src/lib.rs')
        rlib = Path('obj/foo.rlib')
        deps = [Path('../foo/src/other.rs')]
        depfile_contents = [str(d) + ':' for d in deps]
        remote_flag = '--some_forwarded_rewrapper_flag=value'  # not real
        command = _strs(
            [compiler, source, '-o', rlib, f'--remote-flag={remote_flag}'])
        r = rustc_remote_wrapper.RustRemoteAction(
            ['--'] + command,
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )

        mocks = self.generate_prepare_mocks(
            depfile_contents=depfile_contents,
            compiler_shlibs=[shlib_rel],
        )
        with contextlib.ExitStack() as stack:
            for m in mocks:
                stack.enter_context(m)
            prepare_status = r.prepare()

        self.assertEqual(prepare_status, 0)  # success
        a = r.remote_action
        remote_inputs = set(a.inputs_relative_to_working_dir)
        remote_output_files = set(a.output_files_relative_to_working_dir)
        self.assertEqual(
            remote_inputs, set([compiler, shlib_rel, source] + deps))
        self.assertEqual(remote_output_files, {rlib})
        # Position matters, not just presence.
        # Could override the set of standard options.
        self.assertEqual(r.remote_options[-1], remote_flag)

    def test_prepare_environment_names_input_file(self):
        exec_root = Path('/home/project')
        working_dir = exec_root / 'build-here'
        compiler = Path('../tools/bin/rustc')
        shlib = Path('tools/lib/librusteze.so')
        shlib_abs = exec_root / shlib
        shlib_rel = cl_utils.relpath(shlib_abs, start=working_dir)
        source = Path('../foo/src/lib.rs')
        rlib = Path('obj/foo.rlib')
        deps = [Path('../foo/src/other.rs')]
        depfile_contents = [str(d) + ':' for d in deps]
        env_file = Path('../path/to/config.json')
        command = _strs(
            [
                f'CONFIGURE_THINGY={env_file}', compiler, source, '-o', rlib,
                f'--remote-inputs={env_file}'
            ])
        r = rustc_remote_wrapper.RustRemoteAction(
            ['--'] + command,
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )

        mocks = self.generate_prepare_mocks(
            depfile_contents=depfile_contents,
            compiler_shlibs=[shlib_rel],
        )
        with contextlib.ExitStack() as stack:
            for m in mocks:
                stack.enter_context(m)
            # We are not scanning the environment variables automatically.
            # Users should follow the above example command and
            # explicitly pass --remote-inputs.
            with mock.patch.object(rustc_remote_wrapper, '_env_file_exists',
                                   return_value=True) as mock_exists:
                prepare_status = r.prepare()
            mock_exists.assert_not_called()

        self.assertEqual(prepare_status, 0)  # success
        a = r.remote_action
        remote_inputs = set(a.inputs_relative_to_working_dir)
        remote_output_files = set(a.output_files_relative_to_working_dir)
        self.assertEqual(
            remote_inputs,
            set(_paths([compiler, shlib_rel, source, env_file] + deps)))
        self.assertEqual(remote_output_files, {rlib})

    def test_native_compile(self):
        host_platform = fuchsia.REMOTE_PLATFORM
        exec_root = Path('/home/project')
        working_dir = exec_root / 'build-here'
        compiler = Path(f'../tools/{host_platform}/bin/rustc')
        source = Path('../foo/src/lib.rs')
        rlib = Path('obj/foo.rlib')
        command = _strs([compiler, source, '-o', rlib])
        r = rustc_remote_wrapper.RustRemoteAction(
            ['--'] + command,
            exec_root=exec_root,
            working_dir=working_dir,
            host_platform=host_platform,
            auto_reproxy=False,
        )
        self.assertTrue(r.host_matches_remote)

    def test_target_linker_prefix(self):
        for target, ld in [
            ('arm64-unknown-linux-gnu', 'ld'),
            ('x86_64-unknown-linux-gnu', 'ld'),
            ('x86_64-unknown-freebsd', 'ld'),
            ('x86_64-apple-darwin', 'ld64'),
            ('ppc64-apple-darwin', 'ld64'),
        ]:
            exec_root = Path('/home/project')
            working_dir = exec_root / 'build-here'
            compiler = Path('../tools/host-platform/bin/rustc')
            source = Path('../foo/src/lib.rs')
            rlib = Path('obj/foo.rlib')
            command = _strs(
                [compiler, source, '-o', rlib, f'--target={target}'])
            r = rustc_remote_wrapper.RustRemoteAction(
                ['--'] + command,
                exec_root=exec_root,
                working_dir=working_dir,
                auto_reproxy=False,
            )
            self.assertEqual(r.target_linker_prefix, ld)

    def test_remote_mode_ok_with_relative_c_sysroot(self):
        exec_root = Path('/home/project')
        working_dir = exec_root / 'build-here'
        compiler = Path('../tools/host-platform/bin/rustc')
        source = Path('../foo/src/lib.rs')
        rlib = Path('obj/foo.rlib')
        c_sysroot = Path('generated/sys/root')  # relative
        target = 'aarch64-unknown-linux-gnu'
        command = _strs(
            [
                compiler, source, '-o', rlib, '--crate-type=bin',
                f'--target={target}', f'-Clink-arg=--sysroot={c_sysroot}'
            ])
        r = rustc_remote_wrapper.RustRemoteAction(
            ['--'] + command,
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )
        self.assertEqual(r.c_sysroot, c_sysroot)
        self.assertFalse(r._c_sysroot_is_outside_exec_root)
        self.assertTrue(r.needs_linker)
        self.assertFalse(r._remote_disqualified())
        self.assertFalse(r.local_only)
        sysroot_files = list(r._c_sysroot_files())
        self.assertNotEqual(sysroot_files, [])

    def test_forced_local_mode_ok_with_external_absolute_c_sysroot(self):
        exec_root = Path('/home/project')
        working_dir = exec_root / 'build-here'
        compiler = Path('../tools/host-platform/bin/rustc')
        source = Path('../foo/src/lib.rs')
        rlib = Path('obj/foo.rlib')
        c_sysroot = Path('/fancy/pants/local/SDK')  # absolute, external
        target = 'aarch64-unknown-linux-gnu'
        command = _strs(
            [
                compiler, source, '-o', rlib, '--crate-type=bin',
                f'--target={target}', f'-Clink-arg=--sysroot={c_sysroot}'
            ])
        r = rustc_remote_wrapper.RustRemoteAction(
            ['--'] + command,
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )
        self.assertEqual(r.c_sysroot, c_sysroot)
        self.assertTrue(r._c_sysroot_is_outside_exec_root)
        self.assertTrue(r.needs_linker)
        self.assertTrue(r._remote_disqualified())
        self.assertTrue(r.local_only)
        with mock.patch.object(fuchsia, 'c_sysroot_files') as mock_files:
            sysroot_files = list(r._c_sysroot_files())
        self.assertEqual(sysroot_files, [])
        mock_files.assert_not_called()

    def test_cdylib_rust_lld(self):
        exec_root = Path('/home/project')
        working_dir = exec_root / 'build-here'
        compiler = Path('../tools/mac-x64/bin/rustc')
        remote_compiler = Path('../tools/linux-x64/bin/rustc')
        remote_rust_lld = remote_compiler.parent / fuchsia._REMOTE_RUST_LLD_RELPATH
        source = Path('../foo/src/lib.rs')
        dylib = Path('obj/foo.dylib')
        command = _strs(
            [compiler, source, '--crate-type', 'cdylib', '-o', dylib])
        r = rustc_remote_wrapper.RustRemoteAction(
            ['--'] + command,
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )
        mocks = self.generate_prepare_mocks()
        with contextlib.ExitStack() as stack:
            for m in mocks:
                stack.enter_context(m)
            with mock.patch.object(rustc_remote_wrapper.RustRemoteAction,
                                   '_local_depfile_inputs') as mock_deps:
                with mock.patch.object(rustc_remote_wrapper.RustRemoteAction,
                                       '_remote_rustc_shlibs') as mock_shlibs:
                    prepare_status = r.prepare()

        mock_deps.assert_called_once()
        mock_shlibs.assert_called_once()
        a = r.remote_action
        remote_inputs = set(a.inputs_relative_to_working_dir)
        self.assertIn(remote_compiler, remote_inputs)
        self.assertIn(remote_rust_lld, remote_inputs)

    def test_prepare_remote_cross_compile(self):
        host_platform = 'mac-arm64'
        remote_platform = fuchsia.REMOTE_PLATFORM
        exec_root = Path('/home/project')
        working_dir = exec_root / 'build-here'
        compiler = Path(f'../tools/{host_platform}/bin/rustc')
        remote_compiler = Path(f'../tools/{remote_platform}/bin/rustc')
        linker = Path(f'../tools/{host_platform}/bin/linker')
        remote_linker = Path(f'../tools/{remote_platform}/bin/linker')
        remote_ld_path = Path(f'../tools/{remote_platform}/bin/ld.lld')
        shlib = Path('tools/lib/librusteze.so')
        shlib_abs = exec_root / shlib
        shlib_rel = cl_utils.relpath(shlib_abs, start=working_dir)
        libcxx = Path('../std/tools/bin/../lib/libc++.a')
        libcxx_norm = Path('../std/tools/lib/libc++.a')
        target = 'powerpc-unknown-freebsd'
        source = Path('../foo/src/lib.rs')
        rlib = Path('obj/foo.rlib')
        deps = [Path('../foo/src/other.rs')]
        depfile_contents = [str(d) + ':' for d in deps]
        command = _strs(
            [
                compiler, source, '-o', rlib, f'--target={target}',
                f'-Clinker={linker}', f'-Clink-arg={libcxx}',
                f'-Clink-arg=-fuse-ld=lld'
            ])
        r = rustc_remote_wrapper.RustRemoteAction(
            ['--'] + command,
            exec_root=exec_root,
            working_dir=working_dir,
            host_platform=host_platform,
            auto_reproxy=False,
        )
        self.assertEqual(r.working_dir, working_dir)
        self.assertFalse(r.host_matches_remote)
        self.assertEqual(r.original_command, command)
        self.assertEqual(r.linker, linker)
        self.assertEqual(r.remote_linker, remote_linker)
        self.assertEqual(r.remote_ld_path, remote_ld_path)

        mocks = self.generate_prepare_mocks(
            depfile_contents=depfile_contents,
            compiler_shlibs=[shlib_rel],
        )
        with contextlib.ExitStack() as stack:
            for m in mocks:
                stack.enter_context(m)
            prepare_status = r.prepare()
            remote_command = list(r.remote_compile_command())

        self.assertEqual(prepare_status, 0)  # success
        self.assertEqual(r.remote_compiler, remote_compiler)
        a = r.remote_action
        self.assertEqual(
            remote_command,
            _strs(
                [
                    remote_compiler,
                    source,
                    '-o',
                    rlib,
                    f'--target={target}',
                    f'-Clinker={remote_linker}',
                    f'-Clink-arg={libcxx_norm}',  # normalized
                    f'-Clink-arg=-fuse-ld=lld',
                    f'-Clink-arg=--ld-path={remote_ld_path}',  # added by wrapper
                    f'--sysroot=../some/random/sysroot',
                ]))
        remote_inputs = set(a.inputs_relative_to_working_dir)
        remote_output_files = set(a.output_files_relative_to_working_dir)
        self.assertEqual(
            remote_inputs,
            set([remote_compiler, shlib_rel, libcxx, source] + deps))
        self.assertEqual(remote_output_files, {rlib})

    def test_post_run_actions(self):
        exec_root = Path('/home/project')
        working_dir = exec_root / 'build-here'
        compiler = Path('../tools/bin/rustc')
        shlib = Path('tools/lib/librusteze.so')
        shlib_abs = exec_root / shlib
        shlib_rel = cl_utils.relpath(shlib_abs, start=working_dir)
        source = Path('../foo/src/lib.rs')
        rlib = Path('obj/foo.rlib')
        deps = [Path('../foo/src/other.rs')]
        depfile_path = Path('obj/foo.rlib.d')
        depfile_contents = [str(d) + ':' for d in deps]
        command = _strs(
            [compiler, source, '-o', rlib, f'--emit=dep-info={depfile_path}'])
        r = rustc_remote_wrapper.RustRemoteAction(
            ['--'] + command,
            exec_root=exec_root,
            working_dir=working_dir,
            auto_reproxy=False,
        )

        prepare_mocks = self.generate_prepare_mocks(
            depfile_contents=depfile_contents,
            compiler_shlibs=[shlib_rel],
        )
        with contextlib.ExitStack() as stack:
            for m in prepare_mocks:
                stack.enter_context(m)
            prepare_status = r.prepare()

        self.assertEqual(prepare_status, 0)  # success
        a = r.remote_action
        self.assertIsNotNone(a._post_remote_run_success_action)
        remote_inputs = set(a.inputs_relative_to_working_dir)
        remote_output_files = set(a.output_files_relative_to_working_dir)
        self.assertEqual(
            remote_inputs, set([compiler, shlib_rel, source] + deps))
        self.assertEqual(remote_output_files, {rlib, depfile_path})

        run_mocks = [
            mock.patch.object(
                remote_action.RemoteAction,
                '_run_maybe_remotely',
                return_value=cl_utils.SubprocessResult(0)),
            mock.patch.object(
                rustc_remote_wrapper.RustRemoteAction,
                '_depfile_exists',
                return_value=True),
            mock.patch.object(
                rustc_remote_wrapper.RustRemoteAction, '_cleanup'),
        ]
        with contextlib.ExitStack() as stack:
            for m in run_mocks:
                stack.enter_context(m)
            with mock.patch.object(
                    rustc_remote_wrapper.RustRemoteAction,
                    '_rewrite_remote_or_local_depfile') as mock_rewrite:
                run_status = r.run()
            mock_rewrite.assert_called_with()

    def test_rewrite_remote_depfile(self):
        compiler = Path('../tools/bin/rustc')
        source = Path('../foo/src/lib.rs')
        rlib = Path('obj/foo.rlib')
        depfile_path = Path('obj/foo.rlib.d')
        with tempfile.TemporaryDirectory() as td:
            exec_root = Path(td)
            working_dir = exec_root / 'build-here'
            command = _strs(
                [
                    compiler, source, '-o', rlib,
                    f'--emit=dep-info={depfile_path}'
                ])
            r = rustc_remote_wrapper.RustRemoteAction(
                ['--canonicalize_working_dir=true', '--'] + command,
                exec_root=exec_root,
                working_dir=working_dir,
                auto_reproxy=False,
            )

            prepare_mocks = self.generate_prepare_mocks()
            with contextlib.ExitStack() as stack:
                for m in prepare_mocks:
                    stack.enter_context(m)

                with mock.patch.object(rustc_remote_wrapper.RustRemoteAction,
                                       '_remote_inputs',
                                       return_value=iter([])) as mock_inputs:
                    prepare_status = r.prepare()

            mock_inputs.assert_called_with()
            self.assertEqual(prepare_status, 0)  # success
            remote_cwd = r.remote_action.remote_working_dir
            depfile_abspath = working_dir / depfile_path
            depfile_abspath.parent.mkdir(parents=True, exist_ok=True)
            _write_file_contents(
                depfile_abspath,
                f'{remote_cwd}/obj/foo.rlib: {remote_cwd}/src/lib.rs\n')
            r._rewrite_remote_or_local_depfile()
            new_deps = _read_file_contents(depfile_abspath)
            self.assertEqual(new_deps, 'obj/foo.rlib: src/lib.rs\n')


class MainTests(unittest.TestCase):

    def test_help_implicit(self):
        # Just make sure help exits successfully, without any exceptions
        # due to argument parsing.
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            with mock.patch.object(sys, 'exit',
                                   side_effect=ImmediateExit) as mock_exit:
                with self.assertRaises(ImmediateExit):
                    rustc_remote_wrapper.main([])
        mock_exit.assert_called_with(0)

    def test_help_flag(self):
        # Just make sure help exits successfully, without any exceptions
        # due to argument parsing.
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            with mock.patch.object(sys, 'exit',
                                   side_effect=ImmediateExit) as mock_exit:
                with self.assertRaises(ImmediateExit):
                    rustc_remote_wrapper.main(['--help'])
        mock_exit.assert_called_with(0)

    def test_auto_relaunched_with_reproxy(self):
        argv = ['--', 'rustc', 'foo.rs', '-o', 'foo.rlib']
        with mock.patch.object(os.environ, 'get',
                               return_value=None) as mock_env:
            with mock.patch.object(cl_utils, 'exec_relaunch',
                                   side_effect=ImmediateExit) as mock_relaunch:
                with self.assertRaises(ImmediateExit):
                    rustc_remote_wrapper.main(argv)
        mock_env.assert_called()
        mock_relaunch.assert_called_once()
        args, kwargs = mock_relaunch.call_args_list[0]
        relaunch_cmd = args[0]
        self.assertEqual(relaunch_cmd[0], fuchsia.REPROXY_WRAP)
        cmd_slices = cl_utils.split_into_subsequences(relaunch_cmd[1:], '--')
        reproxy_args, self_script, wrapped_command = cmd_slices
        self.assertEqual(reproxy_args, [])
        self.assertIn('python', self_script[0])
        self.assertTrue(self_script[-1].endswith('rustc_remote_wrapper.py'))
        self.assertEqual(wrapped_command, argv[1:])


if __name__ == '__main__':
    unittest.main()
