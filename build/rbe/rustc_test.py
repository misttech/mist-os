#!/usr/bin/env python3.8
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import unittest
from unittest import mock

import rustc


class RustActionTests(unittest.TestCase):

    def test_simple(self):
        r = rustc.RustAction(
            ['../tools/rustc', '../foo/lib.rs', '-o', 'foo.rlib'])
        self.assertEqual(r.compiler, '../tools/rustc')
        self.assertEqual(r.env, [])
        self.assertEqual(r.crate_type, rustc.CrateType.UNKNOWN)
        self.assertEqual(r.output_file, 'foo.rlib')
        self.assertEqual(r.direct_sources, ['../foo/lib.rs'])
        self.assertEqual(r.emit, {})
        self.assertEqual(r.target, '')
        self.assertFalse(r.emit_llvm_ir)
        self.assertFalse(r.emit_llvm_bc)
        self.assertFalse(r.save_analysis)
        self.assertFalse(r.llvm_time_trace)
        self.assertEqual(r.extra_filename, '')
        self.assertEqual(r.depfile, '')
        self.assertEqual(r.linker, '')
        self.assertEqual(r.link_arg_files, [])
        self.assertIsNone(r.link_map_output)
        self.assertEqual(r.sysroot, '')
        self.assertEqual(r.native, [])
        self.assertEqual(r.explicit_link_arg_files, [])
        self.assertEqual(r.externs, {})
        self.assertEqual(set(r.extern_paths()), set())

    def test_env(self):
        r = rustc.RustAction(
            ['A=B', 'C=D', '../tools/rustc', '../foo/lib.rs', '-o', 'foo.rlib'])
        self.assertEqual(r.env, ['A=B', 'C=D'])

    def test_crate_type(self):
        cases = [
            ("rlib", rustc.CrateType.RLIB, False),
            ("bin", rustc.CrateType.BINARY, True),
            ("cdylib", rustc.CrateType.CDYLIB, True),
            ("dylib", rustc.CrateType.DYLIB, True),
            ("proc-macro", rustc.CrateType.PROC_MACRO, True),
            ("staticlib", rustc.CrateType.STATICLIB, False),
        ]
        for k, v, needs_linker in cases:
            r = rustc.RustAction(
                ['../tools/rustc', '--crate-type', k, '-o', 'foo.rlib'])
            self.assertEqual(r.crate_type, v)
            self.assertEqual(r.needs_linker, needs_linker)

    def test_emit_link(self):
        r = rustc.RustAction(
            [
                '../tools/rustc', '--emit=link', '../foo/lib.rs', '-o',
                'foo.rlib'
            ])
        self.assertEqual(r.emit, {'link': []})

    def test_emit_depfile(self):
        r = rustc.RustAction(
            [
                '../tools/rustc', '--emit=dep-info=foo.rlib.d', '../foo/lib.rs',
                '-o', 'foo.rlib'
            ])
        self.assertEqual(r.emit, {'dep-info': ['foo.rlib.d']})
        self.assertEqual(r.depfile, 'foo.rlib.d')

    def test_emit_link_and_depfile(self):
        r = rustc.RustAction(
            [
                '../tools/rustc', '--emit=dep-info=foo.rlib.d,link',
                '../foo/lib.rs', '-o', 'foo.rlib'
            ])
        self.assertEqual(r.emit, {'link': [], 'dep-info': ['foo.rlib.d']})
        self.assertEqual(r.depfile, 'foo.rlib.d')

    def test_emit_llvm_ir(self):
        r = rustc.RustAction(
            [
                '../tools/rustc', '--emit=llvm-ir', '../foo/lib.rs', '-o',
                'obj/foo.rlib'
            ])
        self.assertTrue(r.emit_llvm_ir)
        self.assertFalse(r.emit_llvm_bc)
        self.assertEqual(r.output_file, 'obj/foo.rlib')
        self.assertIn('obj/foo.ll', set(r.extra_output_files()))

    def test_emit_llvm_bc(self):
        r = rustc.RustAction(
            [
                '../tools/rustc', '--emit=llvm-bc', '../foo/lib.rs', '-o',
                'obj/foo.rlib'
            ])
        self.assertTrue(r.emit_llvm_bc)
        self.assertFalse(r.emit_llvm_ir)
        self.assertEqual(r.output_file, 'obj/foo.rlib')
        self.assertIn('obj/foo.bc', set(r.extra_output_files()))

    def test_emit_llvm_ir_bc_extra_filename(self):
        r = rustc.RustAction(
            [
                '../tools/rustc', '--emit=llvm-bc,llvm-ir', '../foo/lib.rs',
                '-o', 'obj/foo.rlib', '-C', 'extra-filename=-feedbeef'
            ])
        self.assertTrue(r.emit_llvm_bc)
        self.assertTrue(r.emit_llvm_ir)
        self.assertEqual(r.output_file, 'obj/foo.rlib')
        self.assertEqual(
            {'obj/foo-feedbeef.bc', 'obj/foo-feedbeef.ll'},
            set(r.extra_output_files()))

    def test_sysroot(self):
        sysroot = '../path/to/platform/sysroot'
        r = rustc.RustAction(
            [
                '../tools/rustc', f'-Clink-arg=--sysroot={sysroot}',
                '../foo/lib.rs', '-o', 'foo.rlib'
            ])
        self.assertEqual(r.sysroot, sysroot)

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
                'save-analysis-temp/foo.json', set(r.extra_output_files()))

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
                'obj/foo.llvm_timings.json', set(r.extra_output_files()))

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
        linker = '../path/to/linky-mc-link-face'
        for opts in (
            [f'-Clinker={linker}'],
            ['-C', f'linker={linker}'],
        ):
            r = rustc.RustAction(
                ['../tools/rustc'] + opts + ['../foo/lib.rs', '-o', 'foo.rlib'])
            self.assertEqual(r.linker, linker)

    def test_link_arg_files(self):
        link_args = ['obj/foo/this.so', 'obj/bar/that.so']
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
        self.assertEqual(r.link_map_output, 'obj/foo.rlib.map')
        self.assertIn('obj/foo.rlib.map', set(r.extra_output_files()))

    def test_direct_inputs(self):
        r = rustc.RustAction(
            [
                '../tools/rustc',
                'obj/foo/this.a',
                '../foo/lib.rs',
                'obj/bar/that.so',
                '-o',
                'foo.rlib',
                'obj/other/foo.dylib',
                'obj/other/bar.so.debug',
                'foo.ld',
            ])
        self.assertEqual(r.direct_sources, ['../foo/lib.rs'])
        self.assertEqual(
            r.explicit_link_arg_files, [
                'obj/foo/this.a', 'obj/bar/that.so', 'obj/other/foo.dylib',
                'obj/other/bar.so.debug', 'foo.ld'
            ])

    def test_Lnative_flag(self):
        link_paths = ['obj/foo/here', 'obj/bar/there']
        for opts in (
            [f'-Lnative={link_paths[0]}', f'-Lnative={link_paths[1]}'],
            ['-L', f'native={link_paths[0]}', '-L', f'native={link_paths[1]}'],
        ):
            r = rustc.RustAction(
                ['../tools/rustc'] + opts + ['../foo/lib.rs', '-o', 'foo.rlib'])
            self.assertEqual(r.native, link_paths)

    def test_externs(self):
        r = rustc.RustAction(
            [
                '../tools/rustc', '--extern', 'this_lib=path/to/this_lib',
                '../foo/lib.rs', '--extern', 'that_lib=path/to/that_lib', '-o',
                'foo.rlib'
            ])
        self.assertEqual(
            r.externs, {
                'this_lib': ['path/to/this_lib'],
                'that_lib': ['path/to/that_lib'],
            })
        self.assertEqual(
            set(r.extern_paths()), {'path/to/this_lib', 'path/to/that_lib'})

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
