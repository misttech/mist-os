#!/usr/bin/env python3.8
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""C++ compilation commands and attributes.
"""

import argparse
import dataclasses
import enum

import cl_utils

from typing import Iterable, Sequence, Tuple

def _remove_suffix(text: str, suffix: str) -> str:
    """string.removesuffix is in Python 3.9+"""
    if text.endswith(suffix):
      return text[:-len(suffix)]
    return text


def _cxx_command_scanner() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Detects C++ compilation attributes",
        argument_default=[],
    )
    parser.add_argument(
        "-o",
        type=str,
        dest="output",
        help="compiler output",
        required=True,
    )
    parser.add_argument(
        "--sysroot",
        type=str,
        dest="sysroot",
        default="",
        help="compiler sysroot",
    )
    parser.add_argument(
        "--target",
        type=str,
        dest="target",
        default="",
        help="target platform",
    )
    parser.add_argument(
        "-fprofile-list",
        type=str,
        dest="profile_list",
        default="",
        help="profile list",
    )
    parser.add_argument(
        "-fcrash-diagnostics-dir",
        type=str,
        dest="crash_diagnostics_dir",
        default="",
        help="additional directory where clang produces crash reports",
    )
    return parser


_CXX_COMMAND_SCANNER = _cxx_command_scanner()


class Compiler(enum.Enum):
    OTHER = 0
    CLANG = 1
    GCC = 2


class SourceLanguage(enum.Enum):
    UNKNOWN = 0
    C = 1
    CXX = 2
    ASM = 3


@dataclasses.dataclass
class Source(object):
    file: str
    dialect: SourceLanguage


def _compile_action_sources(command: Iterable[str]) -> Iterable[Source]:
    for tok in command:
        if tok.endswith('.c'):
            yield Source(file=tok, dialect=SourceLanguage.C)
        if tok.endswith('.cc') or tok.endswith('.cxx') or tok.endswith('.cpp'):
            yield Source(file=tok, dialect=SourceLanguage.CXX)
        if tok.endswith('.s') or tok.endswith('.S'):
            yield Source(file=tok, dialect=SourceLanguage.ASM)


def _infer_dialect_from_sources(sources: Iterable[Source]) -> SourceLanguage:
    # check by source file extension first
    for s in sources:
        return s.dialect

    return SourceLanguage.UNKNOWN


@dataclasses.dataclass
class CompilerTool(object):
    tool: str
    type: Compiler


def _find_compiler_from_command(command: Iterable[str]) -> str:
    for tok in command:
        if 'clang' in tok:  # matches clang++
            return CompilerTool(tool=tok, type=Compiler.CLANG)
        if 'gcc' in tok or 'g++' in tok:
            return CompilerTool(tool=tok, type=Compiler.GCC)
    return None  # or raise error


def _c_preprocess_arg_parser() -> argparse.ArgumentParser:
    """The sole purpose of this is to filter out preprocessing flags.

    We do not actually care about the parameters of these flags.
    """
    parser = argparse.ArgumentParser(
        description="Detects C-preprocessing attributes",
        argument_default=[],
    )
    parser.add_argument(
        "-D",
        type=str,
        dest="defines",
        action='append',
        default=[],
        help="preprocessing defines",
    )
    parser.add_argument(
        "-I",
        type=str,
        dest="includes",
        action='append',
        default=[],
        help="preprocessing include paths",
    )
    parser.add_argument(
        "-L",
        type=str,
        dest="libdirs",
        action='append',
        default=[],
        help="linking search paths",
    )
    parser.add_argument(
        "-U",
        type=str,
        dest="undefines",
        action='append',
        default=[],
        help="preprocessing undefines",
    )
    parser.add_argument(
        "-isystem",
        type=str,
        action='append',
        default=[],
        help="system include paths",
    )
    parser.add_argument(
        "--sysroot",
        type=str,
        help="compiler sysroot",
    )
    parser.add_argument(
        "-stdlib",
        type=str,
        help="C++ standard library",
    )
    parser.add_argument(
        "-include",
        type=str,
        action='append',
        default=[],
        help="prepend include file",
    )
    parser.add_argument(
        "-M",
        action="store_true",
        default=False,
        help="output make rule",
    )
    parser.add_argument(
        "-MM",
        action="store_true",
        default=False,
        help="output make rule without system dirs",
    )
    parser.add_argument(
        "-MG",
        action="store_true",
        default=False,
        help="tolerate missing generated headers",
    )
    parser.add_argument(
        "-MP",
        action="store_true",
        default=False,
        help="deps include phony targets",
    )
    parser.add_argument(
        "-MD",
        action="store_true",
        default=False,
        help="make dependencies",
    )
    parser.add_argument(
        "-MMD",
        action="store_true",
        default=False,
        help="make dependencies, without system headers",
    )
    parser.add_argument(
        "-MF",
        type=str,
        help="name depfile",
    )
    parser.add_argument(
        "-MT",
        type=str,
        help="rename dependency target",
    )
    parser.add_argument(
        "-MQ",
        type=str,
        help="rename dependency target (quoted)",
    )
    parser.add_argument(
        "-undef",
        action="store_true",
        default=False,
        help="no predefined macros",
    )
    return parser

_C_PREPROCESS_ARG_PARSER = _c_preprocess_arg_parser()

_CPP_FUSED_FLAGS = {'-I', '-D', '-L', '-U', '-isystem'}


class CxxAction(object):

    def __init__(self, command: Sequence[str]):
        self._command = command  # keep a copy of the original command
        self._attributes, remaining_args = _CXX_COMMAND_SCANNER.parse_known_args(
            command)
        self._compiler = _find_compiler_from_command(remaining_args)
        self._sources = list(_compile_action_sources(remaining_args))
        self._dialect = _infer_dialect_from_sources(self._sources)

    @property
    def command(self) -> Sequence[str]:
        return self._command

    @property
    def output_file(self) -> str:
        return self._attributes.output  # usually this is the -o file

    @property
    def crash_diagnostics_dir(self) -> str:
        return self._attributes.crash_diagnostics_dir

    @property
    def compiler(self) -> CompilerTool:
        return self._compiler

    @property
    def target(self) -> str:
        return self._attributes.target

    @property
    def sysroot(self) -> str:
        return self._attributes.sysroot

    @property
    def sources(self) -> Sequence[Source]:
        return self._sources

    @property
    def compiler_is_clang(self) -> bool:
        return self.compiler.type == Compiler.CLANG

    @property
    def compiler_is_gcc(self) -> bool:
        return self.compiler.type == Compiler.GCC

    @property
    def dialect_is_c(self) -> bool:
        return self._dialect == SourceLanguage.C

    @property
    def dialect_is_cxx(self) -> bool:
        return self._dialect == SourceLanguage.CXX

    @property
    def preprocessed_output(self) -> str:
        if self._dialect == SourceLanguage.CXX:
            pp_ext = '.ii'
        else:
            pp_ext = '.i'

        return _remove_suffix(self.output_file, '.o') + pp_ext

    @property
    def uses_macos_sdk(self) -> bool:
        return self.sysroot.startswith('/Library/Developer/')

    # TODO: scan command for absolute paths (C++-specific)

    @property
    def _preprocess_only_command(self) -> Iterable[str]:
        # replace the output with a preprocessor output
        for tok in self._command:
            if tok == self.output_file:
                yield self.preprocessed_output
            else:
                # TODO: discard irrelevant flags, like linker flags
                yield tok

        # -E tells the compiler to stop after preprocessing
        yield '-E'

        # -fno-blocks works around in issue where preprocessing includes
        # blocks-featured code when it is not wanted.
        if self.compiler_is_clang:
            yield '-fno-blocks'

    @property
    def _compile_with_preprocessed_input_command(self) -> Iterable[str]:
        canonical_command = cl_utils.expand_fused_flags(self._command, _CPP_FUSED_FLAGS)
        unused_cpp_attributes, remaining_command = _C_PREPROCESS_ARG_PARSER.parse_known_args(canonical_command)

        # replace the first named source file with the preprocessed output
        used_preprocessed_input = False
        for tok in remaining_command:
            if tok.endswith('.c') or tok.endswith('.cc') or tok.endswith(
                    '.cxx') or tok.endswith('.cpp'):
                if used_preprocessed_input:  # ignore other sources after the first
                    continue
                yield self.preprocessed_output
                used_preprocessed_input = True
                continue

            # everything else is kept in the compile command
            yield tok

    def split_preprocessing(self) -> Tuple[Sequence[str], Sequence[str]]:
        """Create separate preprocessing and compile commands.

        Returns:
          C-preprocessing command, and
          modified compile command using preprocessed input
        """
        return list(self._preprocess_only_command), list(self._compile_with_preprocessed_input_command)

# TODO: write a main() routine just for printing debug info about a compile
# command
