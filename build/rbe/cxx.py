#!/usr/bin/env fuchsia-vendored-python
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""C++ compilation commands and attributes.
"""

import argparse
import dataclasses
import enum

import cl_utils

from pathlib import Path
from typing import Iterable, Optional, Sequence, Tuple


def _remove_suffix(text: str, suffix: str) -> str:
    """string.removesuffix is in Python 3.9+"""
    if text.endswith(suffix):
        return text[:-len(suffix)]
    return text


def _cxx_command_scanner() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Detects C++ compilation attributes (clang, gcc)",
        argument_default=[],
        add_help=False,
    )
    parser.add_argument(
        "-o",
        type=Path,
        dest="output",
        default=None,
        metavar="FILE",
        help="compiler output",
        required=True,
    )
    parser.add_argument(
        "--sysroot",
        type=Path,
        default=None,
        help="compiler sysroot",
    )
    parser.add_argument(
        "--target",
        type=str,
        default="",
        help="target platform",
    )
    parser.add_argument(
        "-fprofile-list",
        type=Path,
        dest="profile_list",
        default=None,
        metavar="FILE",
        help="profile list",
    )
    parser.add_argument(
        "-fcrash-diagnostics-dir",
        type=Path,
        dest="crash_diagnostics_dir",
        default=None,
        metavar="DIR",
        help="additional directory where clang produces crash reports",
    )
    parser.add_argument(
        "--save-temps",
        action="store_true",
        default=False,
        help="Compiler saves intermediate files.",
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
    CXX = 2  # C++
    OBJC = 3  # Objective-C
    ASM = 4


@dataclasses.dataclass
class Source(object):
    file: Path
    dialect: SourceLanguage


def _compile_action_sources(command: Iterable[str]) -> Iterable[Source]:
    for tok in command:
        if tok.endswith('.c'):
            yield Source(file=Path(tok), dialect=SourceLanguage.C)
        if tok.endswith('.cc') or tok.endswith('.cxx') or tok.endswith('.cpp'):
            yield Source(file=Path(tok), dialect=SourceLanguage.CXX)
        if tok.endswith('.s') or tok.endswith('.S'):
            yield Source(file=Path(tok), dialect=SourceLanguage.ASM)
        if tok.endswith('.mm'):
            yield Source(file=Path(tok), dialect=SourceLanguage.OBJC)


def _infer_dialect_from_sources(sources: Iterable[Source]) -> SourceLanguage:
    # check by source file extension first
    for s in sources:
        return s.dialect

    return SourceLanguage.UNKNOWN


@dataclasses.dataclass
class CompilerTool(object):
    tool: Path
    type: Compiler


def _find_compiler_from_command(command: Iterable[str]) -> Optional[Path]:
    for tok in command:
        if 'clang' in tok:  # matches clang++
            return CompilerTool(tool=Path(tok), type=Compiler.CLANG)
        # 'g++' matches 'clang++', so this clause must come second:
        if 'gcc' in tok or 'g++' in tok:
            return CompilerTool(tool=Path(tok), type=Compiler.GCC)
    return None  # or raise error


def _c_preprocess_arg_parser() -> argparse.ArgumentParser:
    """The sole purpose of this is to filter out preprocessing flags.

    We do not actually care about the parameters of these flags.
    """
    parser = argparse.ArgumentParser(
        description="Detects C-preprocessing attributes",
        argument_default=[],  # many of the options are repeatable
        add_help=False,
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
        type=Path,
        dest="includes",
        action='append',
        default=[],
        metavar="DIR",
        help="preprocessing include paths",
    )
    parser.add_argument(
        "-L",
        type=Path,
        dest="libdirs",
        action='append',
        default=[],
        metavar="DIR",
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
        type=Path,
        action='append',
        default=[],
        metavar="DIR",
        help="system include paths",
    )
    parser.add_argument(
        "--sysroot",
        type=Path,
        help="compiler sysroot",
    )
    parser.add_argument(
        "-stdlib",
        type=str,
        help="C++ standard library",
    )
    parser.add_argument(
        "-include",
        type=Path,
        action='append',
        default=[],
        metavar="FILE",
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
        type=Path,
        dest="depfile",
        default=None,
        help="name depfile",
        metavar="DEPFILE",
    )
    parser.add_argument(
        "-MT",
        type=Path,
        help="rename dependency target",
    )
    parser.add_argument(
        "-MQ",
        type=Path,
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

# These are flags that are joined with their arguments with
# no separator (no '=' or space).
_CPP_FUSED_FLAGS = {'-I', '-D', '-L', '-U', '-isystem'}


class CxxAction(object):
    """Attributes of C/C++ (or dialect) compilation command.

    Suitable for compilers: clang, gcc
    """

    def __init__(self, command: Sequence[str]):
        self._command = command  # keep a copy of the original command
        self._attributes, remaining_args = _CXX_COMMAND_SCANNER.parse_known_args(
            command)
        self._compiler = _find_compiler_from_command(remaining_args)
        self._sources = list(_compile_action_sources(remaining_args))
        self._dialect = _infer_dialect_from_sources(self._sources)

        canonical_command = cl_utils.expand_fused_flags(
            self._command, _CPP_FUSED_FLAGS)
        self._cpp_attributes, self._command_without_cpp_options = _C_PREPROCESS_ARG_PARSER.parse_known_args(
            canonical_command)

    @property
    def command(self) -> Sequence[str]:
        return self._command

    @property
    def output_file(self) -> Optional[Path]:
        return self._attributes.output  # usually this is the -o file

    @property
    def crash_diagnostics_dir(self) -> Optional[Path]:
        return self._attributes.crash_diagnostics_dir

    @property
    def compiler(self) -> CompilerTool:
        return self._compiler

    @property
    def depfile(self) -> Optional[Path]:
        return self._cpp_attributes.depfile

    @property
    def target(self) -> str:
        return self._attributes.target

    @property
    def sysroot(self) -> Path:
        return self._attributes.sysroot

    @property
    def sources(self) -> Sequence[Source]:
        return self._sources

    @property
    def save_temps(self) -> bool:
        return self._attributes.save_temps

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
    def profile_list(self) -> Optional[Path]:
        return self._attributes.profile_list

    @property
    def preprocessed_suffix(self) -> str:
        if self._dialect == SourceLanguage.CXX:
            return '.ii'
        else:
            return '.i'

    @property
    def preprocessed_output(self) -> Path:
        """This is an explicitly named preprocessed output.

        Depending on the compiler, this may not necessarily match the implicit
        name of the corresponding output from --save-temps.
        """
        # replaces .o with .i or .ii
        return self.output_file.with_suffix(self.preprocessed_suffix)

    @property
    def save_temps_output_stem(self) -> Path:
        """This is the location stem of --save-temps output files (for clang).

        Note that ALL parent dirs and ALL extensions (like .cc.o) get removed.

        Returns:
          A Path stem, to which extensions like .ii can be appended.
        """
        name = self.output_file.name
        return Path(_remove_suffix(name, ''.join(self.output_file.suffixes)))

    @property
    def uses_macos_sdk(self) -> bool:
        return str(self.sysroot).startswith('/Library/Developer/')

    # TODO: scan command for absolute paths (C++-specific)

    def input_files(self) -> Iterable[Path]:
        """Files known to be inputs based on flags."""
        # Note: reclient already infers many C++ inputs in its own
        # input processor, so it is not necessary to list them
        # for remote actions (but it does not hurt).
        for s in self.sources:
            yield s.file
        if self.profile_list:
            yield self.profile_list

    def output_files(self) -> Iterable[Path]:
        # Note: reclient already infers many C++ outputs in its own
        # input processor, so it is not necessary to list them
        # for remote actions (but it does not hurt).
        if self.output_file:
            yield self.output_file  # This should be first, for naming purposes

            if self.save_temps:
                stem = self.save_temps_output_stem
                for suffix in (self.preprocessed_suffix, '.bc', '.s'):
                    yield stem.with_suffix(suffix)

    def output_dirs(self) -> Iterable[Path]:
        if self.crash_diagnostics_dir:
            yield self.crash_diagnostics_dir

    def _preprocess_only_command(self) -> Iterable[str]:
        # replace the output with a preprocessor output
        for tok in self._command:
            if tok == str(self.output_file):
                yield str(self.preprocessed_output)
            elif tok == '--save-temps':  # no need during preprocessing
                pass
            else:
                # TODO: discard irrelevant flags, like linker flags
                yield tok

        # -E tells the compiler to stop after preprocessing
        yield '-E'

        # -fno-blocks works around in issue where preprocessing includes
        # blocks-featured code when it is not wanted.
        if self.compiler_is_clang:
            yield '-fno-blocks'

    def _compile_with_preprocessed_input_command(self) -> Iterable[str]:
        # replace the first named source file with the preprocessed output
        used_preprocessed_input = False
        for tok in self._command_without_cpp_options:
            if tok.endswith('.c') or tok.endswith('.cc') or tok.endswith(
                    '.cxx') or tok.endswith('.cpp'):
                if used_preprocessed_input:  # ignore other sources after the first
                    continue
                yield str(self.preprocessed_output)
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
        return (
            list(self._preprocess_only_command()),
            list(self._compile_with_preprocessed_input_command()),
        )


# TODO: write a main() routine just for printing debug info about a compile
# command
