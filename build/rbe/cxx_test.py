#!/usr/bin/env fuchsia-vendored-python
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import unittest
import sys
from pathlib import Path
from unittest import mock
from typing import Any, Sequence

import cxx


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


class CxxActionTests(unittest.TestCase):

    def test_help_unwanted(self):
        source = Path('hello.cc')
        output = Path('hello.o')
        for opt in (
                '-h',
                '--help',
                '-hwasan-record-stack-history=libcall',
                '-hello-world-is-ignored',
        ):
            with mock.patch.object(sys, 'exit') as mock_exit:
                c = cxx.CxxAction(
                    _strs(['clang++', opt, '-c', source, '-o', output]))
                preprocess, compile = c.split_preprocessing()
            mock_exit.assert_not_called()

    def test_simple_clang_cxx(self):
        source = Path('hello.cc')
        ii_file = Path('hello.ii')
        output = Path('hello.o')
        c = cxx.CxxAction(_strs(['clang++', '-c', source, '-o', output]))
        self.assertEqual(c.output_file, output)
        self.assertEqual(
            c.compiler,
            cxx.CompilerTool(tool=Path('clang++'), type=cxx.Compiler.CLANG))
        self.assertTrue(c.compiler_is_clang)
        self.assertFalse(c.compiler_is_gcc)
        self.assertEqual(c.target, '')
        self.assertEqual(
            c.sources,
            [cxx.Source(file=source, dialect=cxx.SourceLanguage.CXX)])
        self.assertTrue(c.dialect_is_cxx)
        self.assertFalse(c.dialect_is_c)
        self.assertIsNone(c.depfile)
        self.assertIsNone(c.sysroot)
        self.assertIsNone(c.profile_list)
        self.assertEqual(list(c.input_files()), [source])
        self.assertEqual(list(c.output_files()), [output])
        self.assertEqual(list(c.output_dirs()), [])
        self.assertFalse(c.uses_macos_sdk)
        self.assertIsNone(c.crash_diagnostics_dir)
        self.assertEqual(c.preprocessed_output, ii_file)
        preprocess, compile = c.split_preprocessing()
        self.assertEqual(
            preprocess,
            _strs(
                ['clang++', '-c', source, '-o', ii_file, '-E', '-fno-blocks']),
        )
        self.assertEqual(
            compile,
            _strs(['clang++', '-c', ii_file, '-o', output]),
        )

    def test_simple_clang_c(self):
        source = Path('hello.c')
        i_file = Path('hello.i')
        output = Path('hello.o')
        c = cxx.CxxAction(_strs(['clang', '-c', source, '-o', output]))
        self.assertEqual(c.output_file, output)
        self.assertEqual(
            c.compiler,
            cxx.CompilerTool(tool=Path('clang'), type=cxx.Compiler.CLANG))
        self.assertTrue(c.compiler_is_clang)
        self.assertFalse(c.compiler_is_gcc)
        self.assertEqual(c.target, '')
        self.assertEqual(
            c.sources, [cxx.Source(file=source, dialect=cxx.SourceLanguage.C)])
        self.assertFalse(c.dialect_is_cxx)
        self.assertTrue(c.dialect_is_c)
        self.assertIsNone(c.depfile)
        self.assertIsNone(c.sysroot)
        self.assertIsNone(c.profile_list)
        self.assertEqual(list(c.input_files()), [source])
        self.assertEqual(list(c.output_files()), [output])
        self.assertEqual(list(c.output_dirs()), [])
        self.assertIsNone(c.crash_diagnostics_dir)
        self.assertEqual(c.preprocessed_output, i_file)
        preprocess, compile = c.split_preprocessing()
        self.assertEqual(
            preprocess,
            _strs(['clang', '-c', source, '-o', i_file, '-E', '-fno-blocks']),
        )
        self.assertEqual(
            compile,
            _strs(['clang', '-c', i_file, '-o', output]),
        )

    def test_simple_clang_asm(self):
        source = Path('hello.s')
        i_file = Path('hello.i')
        output = Path('hello.o')
        c = cxx.CxxAction(_strs(['clang', '-c', source, '-o', output]))
        self.assertEqual(c.output_file, output)
        self.assertEqual(
            c.compiler,
            cxx.CompilerTool(tool=Path('clang'), type=cxx.Compiler.CLANG))
        self.assertTrue(c.compiler_is_clang)
        self.assertFalse(c.compiler_is_gcc)
        self.assertEqual(c.target, '')
        self.assertEqual(
            c.sources,
            [cxx.Source(file=source, dialect=cxx.SourceLanguage.ASM)])
        self.assertFalse(c.dialect_is_cxx)
        self.assertFalse(c.dialect_is_c)
        self.assertIsNone(c.crash_diagnostics_dir)

    def test_simple_clang_asm_pp(self):
        source = Path('hello.S')
        i_file = Path('hello.i')
        output = Path('hello.o')
        c = cxx.CxxAction(_strs(['clang', '-c', source, '-o', output]))
        self.assertEqual(c.output_file, output)
        self.assertEqual(
            c.compiler,
            cxx.CompilerTool(tool=Path('clang'), type=cxx.Compiler.CLANG))
        self.assertTrue(c.compiler_is_clang)
        self.assertFalse(c.compiler_is_gcc)
        self.assertEqual(c.target, '')
        self.assertEqual(
            c.sources,
            [cxx.Source(file=source, dialect=cxx.SourceLanguage.ASM)])
        self.assertFalse(c.dialect_is_cxx)
        self.assertFalse(c.dialect_is_c)
        self.assertIsNone(c.crash_diagnostics_dir)

    def test_simple_gcc_cxx(self):
        source = Path('hello.cc')
        output = Path('hello.o')
        ii_file = Path('hello.ii')
        c = cxx.CxxAction(_strs(['g++', '-c', source, '-o', output]))
        self.assertEqual(c.output_file, output)
        self.assertEqual(
            c.compiler,
            cxx.CompilerTool(tool=Path('g++'), type=cxx.Compiler.GCC))
        self.assertFalse(c.compiler_is_clang)
        self.assertTrue(c.compiler_is_gcc)
        self.assertEqual(c.target, '')
        self.assertEqual(
            c.sources,
            [cxx.Source(file=source, dialect=cxx.SourceLanguage.CXX)])
        self.assertTrue(c.dialect_is_cxx)
        self.assertFalse(c.dialect_is_c)
        self.assertIsNone(c.depfile)
        self.assertIsNone(c.crash_diagnostics_dir)
        self.assertEqual(c.preprocessed_output, ii_file)
        preprocess, compile = c.split_preprocessing()
        self.assertEqual(
            preprocess,
            _strs(['g++', '-c', source, '-o', ii_file, '-E']),
        )
        self.assertEqual(
            compile,
            _strs(['g++', '-c', ii_file, '-o', output]),
        )

    def test_simple_gcc_c(self):
        source = Path('hello.c')
        i_file = Path('hello.i')
        output = Path('hello.o')
        c = cxx.CxxAction(_strs(['gcc', '-c', source, '-o', output]))
        self.assertEqual(c.output_file, output)
        self.assertEqual(
            c.compiler,
            cxx.CompilerTool(tool=Path('gcc'), type=cxx.Compiler.GCC))
        self.assertFalse(c.compiler_is_clang)
        self.assertTrue(c.compiler_is_gcc)
        self.assertEqual(c.target, '')
        self.assertEqual(
            c.sources, [cxx.Source(file=source, dialect=cxx.SourceLanguage.C)])
        self.assertFalse(c.dialect_is_cxx)
        self.assertTrue(c.dialect_is_c)
        self.assertIsNone(c.depfile)
        self.assertIsNone(c.crash_diagnostics_dir)
        self.assertEqual(c.preprocessed_output, i_file)
        preprocess, compile = c.split_preprocessing()
        self.assertEqual(
            preprocess,
            _strs(['gcc', '-c', source, '-o', i_file, '-E']),
        )
        self.assertEqual(
            compile,
            _strs(['gcc', '-c', i_file, '-o', output]),
        )

    def test_clang_target(self):
        c = cxx.CxxAction(
            [
                'clang++', '--target=powerpc-apple-darwin8', '-c', 'hello.cc',
                '-o', 'hello.o'
            ])
        self.assertEqual(c.target, 'powerpc-apple-darwin8')

    def test_clang_crash_diagnostics_dir(self):
        crash_dir = Path('/tmp/graveyard')
        c = cxx.CxxAction(
            [
                'clang++', f'-fcrash-diagnostics-dir={crash_dir}', '-c',
                'hello.cc', '-o', 'hello.o'
            ])
        self.assertEqual(c.crash_diagnostics_dir, crash_dir)
        self.assertEqual(set(c.output_dirs()), {crash_dir})

    def test_profile_list(self):
        source = Path('hello.cc')
        ii_file = Path('hello.ii')
        output = Path('hello.o')
        profile = Path('my/online/profile.list')
        c = cxx.CxxAction(
            _strs(
                [
                    'clang++', '-c', source, '-o', output,
                    f'-fprofile-list={profile}'
                ]))
        self.assertEqual(c.profile_list, profile)
        self.assertEqual(set(c.input_files()), {source, profile})

    def test_uses_macos_sdk(self):
        sysroot = Path('/Library/Developer/blah')
        c = cxx.CxxAction(
            [
                'clang++', f'--sysroot={sysroot}', '-c', 'hello.cc', '-o',
                'hello.o'
            ])
        self.assertEqual(c.sysroot, sysroot)
        self.assertTrue(c.uses_macos_sdk)

    def test_split_preprocessing(self):
        source = Path('hello.cc')
        ii_file = Path('hello.ii')
        output = Path('hello.o')
        depfile = Path('hello.d')
        c = cxx.CxxAction(
            _strs(
                [
                    'clang++', '-DNDEBUG', '-I/opt/include', '-stdlib=libc++',
                    '-M', '-MF', depfile, '-c', source, '-o', output
                ]))
        self.assertEqual(c.depfile, depfile)
        preprocess, compile = c.split_preprocessing()
        self.assertEqual(
            preprocess,
            _strs(
                [
                    'clang++', '-DNDEBUG', '-I/opt/include', '-stdlib=libc++',
                    '-M', '-MF', depfile, '-c', source, '-o', ii_file, '-E',
                    '-fno-blocks'
                ]),
        )
        self.assertEqual(
            compile,
            _strs(['clang++', '-c', ii_file, '-o', output]),
        )


if __name__ == '__main__':
    unittest.main()
