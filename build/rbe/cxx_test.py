#!/usr/bin/env python3.8
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import unittest
from unittest import mock

import cxx


class CxxActionTests(unittest.TestCase):

    def test_simple_clang_cxx(self):
        c = cxx.CxxAction(['clang++', '-c', 'hello.cc', '-o', 'hello.o'])
        self.assertEqual(c.output_file, 'hello.o')
        self.assertEqual(
            c.compiler,
            cxx.CompilerTool(tool='clang++', type=cxx.Compiler.CLANG))
        self.assertTrue(c.compiler_is_clang)
        self.assertFalse(c.compiler_is_gcc)
        self.assertEqual(c.target, '')
        self.assertEqual(
            c.sources,
            [cxx.Source(file='hello.cc', dialect=cxx.SourceLanguage.CXX)])
        self.assertTrue(c.dialect_is_cxx)
        self.assertFalse(c.dialect_is_c)
        self.assertEqual(c.sysroot, '')
        self.assertFalse(c.uses_macos_sdk)
        self.assertEqual(c.crash_diagnostics_dir, '')
        self.assertEqual(c.preprocessed_output, 'hello.ii')
        preprocess, compile = c.split_preprocessing()
        self.assertEqual(
            preprocess,
            [
                'clang++', '-c', 'hello.cc', '-o', 'hello.ii', '-E',
                '-fno-blocks'
            ],
        )
        self.assertEqual(
            compile,
            ['clang++', '-c', 'hello.ii', '-o', 'hello.o'],
        )

    def test_simple_clang_c(self):
        c = cxx.CxxAction(['clang', '-c', 'hello.c', '-o', 'hello.o'])
        self.assertEqual(c.output_file, 'hello.o')
        self.assertEqual(
            c.compiler, cxx.CompilerTool(tool='clang', type=cxx.Compiler.CLANG))
        self.assertTrue(c.compiler_is_clang)
        self.assertFalse(c.compiler_is_gcc)
        self.assertEqual(c.target, '')
        self.assertEqual(
            c.sources,
            [cxx.Source(file='hello.c', dialect=cxx.SourceLanguage.C)])
        self.assertFalse(c.dialect_is_cxx)
        self.assertTrue(c.dialect_is_c)
        self.assertEqual(c.crash_diagnostics_dir, '')
        self.assertEqual(c.preprocessed_output, 'hello.i')
        preprocess, compile = c.split_preprocessing()
        self.assertEqual(
            preprocess,
            ['clang', '-c', 'hello.c', '-o', 'hello.i', '-E', '-fno-blocks'],
        )
        self.assertEqual(
            compile,
            ['clang', '-c', 'hello.i', '-o', 'hello.o'],
        )

    def test_simple_gcc_cxx(self):
        c = cxx.CxxAction(['g++', '-c', 'hello.cc', '-o', 'hello.o'])
        self.assertEqual(c.output_file, 'hello.o')
        self.assertEqual(
            c.compiler, cxx.CompilerTool(tool='g++', type=cxx.Compiler.GCC))
        self.assertFalse(c.compiler_is_clang)
        self.assertTrue(c.compiler_is_gcc)
        self.assertEqual(c.target, '')
        self.assertEqual(
            c.sources,
            [cxx.Source(file='hello.cc', dialect=cxx.SourceLanguage.CXX)])
        self.assertTrue(c.dialect_is_cxx)
        self.assertFalse(c.dialect_is_c)
        self.assertEqual(c.crash_diagnostics_dir, '')
        self.assertEqual(c.preprocessed_output, 'hello.ii')
        preprocess, compile = c.split_preprocessing()
        self.assertEqual(
            preprocess,
            ['g++', '-c', 'hello.cc', '-o', 'hello.ii', '-E'],
        )
        self.assertEqual(
            compile,
            ['g++', '-c', 'hello.ii', '-o', 'hello.o'],
        )

    def test_simple_gcc_c(self):
        c = cxx.CxxAction(['gcc', '-c', 'hello.c', '-o', 'hello.o'])
        self.assertEqual(c.output_file, 'hello.o')
        self.assertEqual(
            c.compiler, cxx.CompilerTool(tool='gcc', type=cxx.Compiler.GCC))
        self.assertFalse(c.compiler_is_clang)
        self.assertTrue(c.compiler_is_gcc)
        self.assertEqual(c.target, '')
        self.assertEqual(
            c.sources,
            [cxx.Source(file='hello.c', dialect=cxx.SourceLanguage.C)])
        self.assertFalse(c.dialect_is_cxx)
        self.assertTrue(c.dialect_is_c)
        self.assertEqual(c.crash_diagnostics_dir, '')
        self.assertEqual(c.preprocessed_output, 'hello.i')
        preprocess, compile = c.split_preprocessing()
        self.assertEqual(
            preprocess,
            ['gcc', '-c', 'hello.c', '-o', 'hello.i', '-E'],
        )
        self.assertEqual(
            compile,
            ['gcc', '-c', 'hello.i', '-o', 'hello.o'],
        )

    def test_clang_target(self):
        c = cxx.CxxAction(
            [
                'clang++', '--target=powerpc-apple-darwin8', '-c', 'hello.cc',
                '-o', 'hello.o'
            ])
        self.assertEqual(c.target, 'powerpc-apple-darwin8')

    def test_clang_crash_diagnostics_dir(self):
        c = cxx.CxxAction(
            [
                'clang++', '-fcrash-diagnostics-dir=/tmp/graveyard', '-c',
                'hello.cc', '-o', 'hello.o'
            ])
        self.assertEqual(c.crash_diagnostics_dir, '/tmp/graveyard')

    def test_uses_macos_sdk(self):
        c = cxx.CxxAction(
            [
                'clang++', '--sysroot=/Library/Developer/blah', '-c',
                'hello.cc', '-o', 'hello.o'
            ])
        self.assertEqual(c.sysroot, '/Library/Developer/blah')
        self.assertTrue(c.uses_macos_sdk)

    def test_split_preprocessing(self):
        c = cxx.CxxAction(
            [
                'clang++', '-DNDEBUG', '-I/opt/include', '-stdlib=libc++', '-M',
                '-MF', 'hello.d', '-c', 'hello.cc', '-o', 'hello.o'
            ])
        preprocess, compile = c.split_preprocessing()
        self.assertEqual(
            preprocess,
            [
                'clang++', '-DNDEBUG', '-I/opt/include', '-stdlib=libc++', '-M',
                '-MF', 'hello.d', '-c', 'hello.cc', '-o', 'hello.ii', '-E',
                '-fno-blocks'
            ],
        )
        self.assertEqual(
            compile,
            ['clang++', '-c', 'hello.ii', '-o', 'hello.o'],
        )


if __name__ == '__main__':
    unittest.main()
