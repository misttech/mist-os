#!/usr/bin/env fuchsia-vendored-python
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import unittest
import sys
from unittest import mock
from pathlib import Path

import rustc

import cl_utils

from typing import Any, Sequence


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


class RustActionTests(unittest.TestCase):

    def test_simple(self):
        compiler = Path('../tools/rustc')
        source = Path('../foo/lib.rs')
        output = Path('foo.rlib')
        r = rustc.RustAction(_strs([compiler, source, '-o', output]))
        self.assertEqual(r.compiler, compiler)
        self.assertEqual(r.env, [])
        self.assertEqual(r.crate_type, rustc.CrateType.UNKNOWN)
        self.assertEqual(r.output_file, output)
        self.assertEqual(r._output_file_base, 'foo')
        self.assertEqual(r._auxiliary_output_path, 'foo')
        self.assertEqual(r.direct_sources, [source])
        self.assertEqual(r.emit, {})
        self.assertIsNone(r.target)
        self.assertFalse(r.emit_llvm_ir)
        self.assertFalse(r.emit_llvm_bc)
        self.assertFalse(r.save_analysis)
        self.assertFalse(r.llvm_time_trace)
        self.assertEqual(r.extra_filename, '')
        self.assertIsNone(r.depfile)
        self.assertIsNone(r.linker)
        self.assertEqual(r.link_arg_files, [])
        self.assertIsNone(r.link_map_output)
        self.assertIsNone(r.c_sysroot)
        self.assertIsNone(r.use_ld)
        self.assertEqual(r.native, [])
        self.assertEqual(r.explicit_link_arg_files, [])
        self.assertEqual(r.externs, {})
        self.assertEqual(set(r.extern_paths()), set())

    def test_executable_suffixed(self):
        compiler = Path('../tools/rustc')
        source = Path('../foo/lib.rs')
        for output in (Path('bin/foo.exe'), Path('bin/foo')):
            r = rustc.RustAction(
                _strs([compiler, source, '--crate-type=bin', '-o', output]))
            self.assertEqual(r.compiler, compiler)
            self.assertEqual(r.env, [])
            self.assertEqual(r.crate_type, rustc.CrateType.BINARY)
            self.assertEqual(r.output_file, output)
            self.assertEqual(r._output_file_base, 'bin/foo')
            self.assertEqual(r._auxiliary_output_path, 'bin/foo')
            self.assertEqual(r.direct_sources, [source])

    def test_no_source(self):
        compiler = Path('../tools/bin/rustc')
        output = Path('foo.rlib')
        r = rustc.RustAction(_strs([compiler, '-o', output]))
        self.assertEqual(r.direct_sources, [])

    def test_no_output(self):
        compiler = Path('../tools/bin/rustc')
        source = Path('../foo/lib.rs')
        r = rustc.RustAction(_strs([compiler, source]))
        with self.assertRaises(RuntimeError):
            base = r._output_file_base

    def test_do_not_help(self):
        compiler = Path('../tools/rustc')
        source = Path('../foo/lib.rs')
        output = Path('foo.rlib')
        for opt in ('-h', '--help', '-halp-ignore=foo'):
            with mock.patch.object(sys, 'exit') as mock_exit:
                r = rustc.RustAction(
                    _strs([compiler, opt, source, '-o', output]))
                self.assertEqual(r.compiler, compiler)
            mock_exit.assert_not_called()

    def test_env(self):
        r = rustc.RustAction(
            ['A=B', 'C=D', '../tools/rustc', '../foo/lib.rs', '-o', 'foo.rlib'])
        self.assertEqual(r.env, ['A=B', 'C=D'])

    def test_crate_type(self):
        cases = [
            ("rlib", rustc.CrateType.RLIB, False, False),
            ("bin", rustc.CrateType.BINARY, True, True),
            ("cdylib", rustc.CrateType.CDYLIB, True, True),
            ("dylib", rustc.CrateType.DYLIB, True, True),
            ("proc-macro", rustc.CrateType.PROC_MACRO, True, False),
            ("staticlib", rustc.CrateType.STATICLIB, False, False),
        ]
        for k, v, needs_linker, is_executable in cases:
            r = rustc.RustAction(
                ['../tools/rustc', '--crate-type', k, '-o', 'foo.rlib'])
            self.assertEqual(r.crate_type, v)
            self.assertEqual(r.needs_linker, needs_linker)
            self.assertEqual(r.main_output_is_executable, is_executable)

    def test_emit_link(self):
        r = rustc.RustAction(
            [
                '../tools/rustc', '--emit=link', '../foo/lib.rs', '-o',
                'foo.rlib'
            ])
        self.assertEqual(r.emit, {'link': []})

    def test_emit_depfile(self):
        depfile = Path('foo.rlib.d')
        r = rustc.RustAction(
            [
                '../tools/rustc', f'--emit=dep-info={depfile}', '../foo/lib.rs',
                '-o', 'foo.rlib'
            ])
        self.assertEqual(r.emit, {'dep-info': [str(depfile)]})
        self.assertEqual(r.depfile, depfile)

    def test_emit_link_and_depfile(self):
        depfile = Path('foo.rlib.d')
        r = rustc.RustAction(
            [
                '../tools/rustc', f'--emit=dep-info={depfile},link',
                '../foo/lib.rs', '-o', 'foo.rlib'
            ])
        self.assertEqual(r.emit, {'link': [], 'dep-info': [str(depfile)]})
        self.assertEqual(r.depfile, depfile)

    def test_emit_llvm_ir(self):
        output = Path('obj/foo.rlib')
        r = rustc.RustAction(
            [
                '../tools/rustc', '--emit=llvm-ir', '../foo/lib.rs', '-o',
                str(output)
            ])
        self.assertTrue(r.emit_llvm_ir)
        self.assertFalse(r.emit_llvm_bc)
        self.assertEqual(r.output_file, output)
        self.assertIn(Path('obj/foo.ll'), set(r.extra_output_files()))

    def test_emit_llvm_bc(self):
        output = Path('obj/foo.rlib')
        r = rustc.RustAction(
            [
                '../tools/rustc', '--emit=llvm-bc', '../foo/lib.rs', '-o',
                str(output)
            ])
        self.assertTrue(r.emit_llvm_bc)
        self.assertFalse(r.emit_llvm_ir)
        self.assertEqual(r.output_file, output)
        self.assertIn(Path('obj/foo.bc'), set(r.extra_output_files()))

    def test_emit_llvm_ir_bc_extra_filename(self):
        output = Path('obj/foo.rlib')
        r = rustc.RustAction(
            [
                '../tools/rustc', '--emit=llvm-bc,llvm-ir', '../foo/lib.rs',
                '-o',
                str(output), '-C', 'extra-filename=-feedbeef'
            ])
        self.assertTrue(r.emit_llvm_bc)
        self.assertTrue(r.emit_llvm_ir)
        self.assertEqual(r.output_file, output)
        self.assertEqual(
            _paths({'obj/foo-feedbeef.bc', 'obj/foo-feedbeef.ll'}),
            set(r.extra_output_files()))

    def test_c_sysroot(self):
        c_sysroot = Path('../path/to/platform/sysroot')
        r = rustc.RustAction(
            [
                '../tools/rustc', f'-Clink-arg=--sysroot={c_sysroot}',
                '../foo/lib.rs', '-o', 'foo.rlib'
            ])
        self.assertEqual(r.c_sysroot, c_sysroot)

    def test_rust_sysroot(self):
        rust_sysroot = Path('../path/to/rust/sysroot')
        r = rustc.RustAction(
            [
                '../tools/rustc', '--sysroot',
                str(rust_sysroot), '../foo/lib.rs', '-o', 'foo.rlib'
            ])
        self.assertEqual(r.rust_sysroot, rust_sysroot)

    def test_fuse_ld(self):
        ld = Path('gold')
        r = rustc.RustAction(
            [
                '../tools/rustc', f'-Clink-arg=-fuse-ld={ld}', '../foo/lib.rs',
                '-o', 'foo.rlib'
            ])
        self.assertEqual(r.use_ld, ld)

    def test_rust_sysroot_default(self):
        working_dir = Path('/home/project/build')
        fake_default_sysroot = Path('/home/project/tools/fake/sysroot')
        compiler = Path('../tools/rustc')
        r = rustc.RustAction(
            [str(compiler), '../foo/lib.rs', '-o', 'foo.rlib'],
            working_dir=working_dir)
        call_result = cl_utils.SubprocessResult(
            returncode=0,
            stdout=[f'{fake_default_sysroot}\n'],
        )
        with mock.patch.object(cl_utils, 'subprocess_call',
                               return_value=call_result) as mock_call:
            self.assertEqual(
                r.default_rust_sysroot(), Path('../tools/fake/sysroot'))
        mock_call.assert_called_with(
            [str(compiler), '--print', 'sysroot'], cwd=working_dir, quiet=True)

        with mock.patch.object(
                rustc.RustAction, 'default_rust_sysroot',
                return_value=fake_default_sysroot) as mock_default:
            self.assertEqual(r.rust_sysroot, fake_default_sysroot)
        mock_default.assert_called_with()

    def test_target(self):
        target = f'--target=powerpc32-apple-darwin8'
        r = rustc.RustAction(
            [
                '../tools/rustc', f'--target={target}', '../foo/lib.rs', '-o',
                'foo.rlib'
            ])
        self.assertEqual(r.target, target)

    def test_save_analysis(self):
        for opts in (
            ['-Zsave-analysis=yes'],
            ['-Z', 'save-analysis=yes'],
        ):
            r = rustc.RustAction(
                ['../tools/rustc'] + opts +
                ['../foo/lib.rs', '-o', 'obj/foo.rlib'])
            self.assertTrue(r.save_analysis)
            self.assertIn(
                Path('save-analysis-temp/foo.json'),
                set(r.extra_output_files()))

    def test_llvm_time_trace(self):
        for opts in (
            ['-Zllvm-time-trace'],
            ['-Z', 'llvm-time-trace'],
        ):
            r = rustc.RustAction(
                ['../tools/rustc'] + opts +
                ['../foo/lib.rs', '-o', 'obj/foo.rlib'])
            self.assertTrue(r.llvm_time_trace)
            self.assertIn(
                Path('obj/foo.llvm_timings.json'), set(r.extra_output_files()))

    def test_extra_filename(self):
        suffix = 'cafef00d'
        for opts in (
            [f'-Cextra-filename={suffix}'],
            ['-C', f'extra-filename={suffix}'],
        ):
            r = rustc.RustAction(
                ['../tools/rustc'] + opts + ['../foo/lib.rs', '-o', 'foo.rlib'])
            self.assertEqual(r.extra_filename, suffix)

    def test_linker(self):
        linker = Path('../path/to/linky-mc-link-face')
        for opts in (
            [f'-Clinker={linker}'],
            ['-C', f'linker={linker}'],
        ):
            r = rustc.RustAction(
                ['../tools/rustc'] + opts + ['../foo/lib.rs', '-o', 'foo.rlib'])
            self.assertEqual(r.linker, linker)

    def test_link_arg_files(self):
        link_args = _paths(['obj/foo/this.so', 'obj/bar/that.so'])
        for opts in (
            [f'-Clink-arg={link_args[0]}', f'-Clink-arg={link_args[1]}'],
            ['-C', f'link-arg={link_args[0]}', '-C',
             f'link-arg={link_args[1]}'],
        ):
            r = rustc.RustAction(
                ['../tools/rustc'] + opts + ['../foo/lib.rs', '-o', 'foo.rlib'])
            self.assertEqual(r.link_arg_files, link_args)

    def test_link_map_output(self):
        r = rustc.RustAction(
            [
                '../tools/rustc', '../foo/lib.rs', '-o', 'foo.rlib',
                '-Clink-args=--Map=./obj/foo.rlib.map'
            ])
        map_file = Path('obj/foo.rlib.map')
        self.assertEqual(r.link_map_output, map_file)
        self.assertIn(map_file, set(r.extra_output_files()))

    def test_direct_inputs(self):
        input1 = Path('obj/foo/this.a')
        input2 = Path('../foo/lib.rs')
        input3 = Path('obj/bar/that.so')
        input4 = Path('obj/other/foo.dylib')
        input5 = Path('obj/other/bar.so.debug')
        input6 = Path('foo.ld')
        output = Path('foo.rlib')
        r = rustc.RustAction(
            _strs(
                [
                    '../tools/rustc',
                    input1,
                    input2,
                    input3,
                    '-o',
                    output,
                    input4,
                    input5,
                    input6,
                ]))
        self.assertEqual(r.direct_sources, [input2])
        self.assertEqual(
            r.explicit_link_arg_files, [input1, input3, input4, input5, input6])

    def test_Lnative_flag(self):
        link_paths = _paths(['obj/foo/here', 'obj/bar/there'])
        for opts in (
            [f'-Lnative={link_paths[0]}', f'-Lnative={link_paths[1]}'],
            ['-L', f'native={link_paths[0]}', '-L', f'native={link_paths[1]}'],
        ):
            r = rustc.RustAction(
                ['../tools/rustc'] + opts + ['../foo/lib.rs', '-o', 'foo.rlib'])
            self.assertEqual(r.native, link_paths)

    def test_externs(self):
        libdir1 = Path('path/to/this_lib')
        libdir2 = Path('path/to/that_lib')
        r = rustc.RustAction(
            [
                '../tools/rustc', '--extern', f'this_lib={libdir1}',
                '../foo/lib.rs', '--extern', f'that_lib={libdir2}', '-o',
                'foo.rlib'
            ])
        self.assertEqual(
            r.externs, {
                'this_lib': libdir1,
                'that_lib': libdir2,
            })
        self.assertEqual(
            set(r.extern_paths()),
            _paths({'path/to/this_lib', 'path/to/that_lib'}))

    def test_externs_last_wins(self):
        libdir1 = Path('path/to/this_lib')
        libdir2 = Path('path/to/that_lib')
        r = rustc.RustAction(
            [
                '../tools/rustc', '--extern', f'this_lib={libdir1}',
                '../foo/lib.rs', '--extern', f'this_lib={libdir2}', '-o',
                'foo.rlib'
            ])
        self.assertEqual(r.externs, {
            'this_lib': libdir2,
        })
        self.assertEqual(set(r.extern_paths()), _paths({'path/to/that_lib'}))

    def test_dep_only_command(self):
        new_depfile = 'some/where/new-foo.rlib.d.other'
        emit_opts = (
            ['--emit=link'],
            ['--emit=dep-info=not-going-to-use-this.d'],
            ['--emit=link', '--emit=dep-info=not-going-to-use-this.d'],
            ['--emit=link,dep-info=not-going-to-use-this.d'],
            ['--emit=dep-info=not-going-to-use-this.d,link'],
        )
        for opts in emit_opts:
            r = rustc.RustAction(
                [
                    '../tools/rustc',
                ] + opts + ['../foo/lib.rs', '-o', 'foo.rlib'])
            self.assertEqual(
                list(r.dep_only_command(new_depfile)), [
                    '../tools/rustc', f'--emit=dep-info={new_depfile}', '-Z',
                    'binary-dep-depinfo', '../foo/lib.rs', '-o', 'foo.rlib'
                ])

            r = rustc.RustAction(
                ['../tools/rustc', '../foo/lib.rs', '-o', 'foo.rlib'])
            self.assertEqual(
                list(r.dep_only_command(new_depfile)), [
                    '../tools/rustc', '../foo/lib.rs', '-o', 'foo.rlib',
                    f'--emit=dep-info={new_depfile}', '-Z', 'binary-dep-depinfo'
                ])


if __name__ == '__main__':
    unittest.main()
