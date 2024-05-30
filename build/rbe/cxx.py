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
from typing import AbstractSet, Iterable, Optional, Sequence, Tuple


def _remove_suffix(text: str, suffix: str) -> str:
    """string.removesuffix is in Python 3.9+"""
    if text.endswith(suffix):
        return text[: -len(suffix)]
    return text


def _cxx_command_scanner() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Detects C++ compilation attributes (clang, gcc)",
        argument_default=[],
        allow_abbrev=False,
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
        "-L",
        type=Path,
        dest="libdirs",
        action="append",
        default=[],
        metavar="DIR",
        help="linking search paths",
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
        "-shared",
        default=False,
        action="store_true",
        help="output is linked shared",
    )
    parser.add_argument(
        "-fprofile-generate",
        dest="profile_generate",
        type=Path,
        default=None,
        help="generate profiling instrumented code (named directory)",
        metavar="DIR",
    )
    parser.add_argument(
        "-fprofile-instr-generate",
        dest="profile_instr_generate",
        type=Path,
        default=None,
        help="generate profiling instrumented code (named file)",
        metavar="FILE",
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
        "-fsanitize",
        type=str,
        action=cl_utils.StringSetAdd,
        dest="sanitize",
        default=set(),
        help="Compiler/linker sanitize options (cumulative)",
    )
    parser.add_argument(
        "-fno-sanitize",
        type=str,
        action=cl_utils.StringSetRemove,
        dest="sanitize",
        default=set(),
        help="Disable compiler/linker sanitize options",
    )
    parser.add_argument(
        "-flto",
        type=str,
        dest="lto",
        default=None,
        nargs="?",  # default: full LTO
        help="Link time optimization",
    )
    parser.add_argument(
        "-fuse-ld",
        type=str,
        dest="use_ld",
        default=None,
        metavar="LD",
        help="underyling linker tool",
    )
    parser.add_argument(
        "--save-temps",
        action="store_true",
        default=False,
        help="Compiler saves intermediate files.",
    )
    parser.add_argument(
        "-unwindlib",
        type=str,
        default=None,
        metavar="LIB",
        help="unwind library (e.g. libunwind)",
    )
    parser.add_argument(
        "-rtlib",
        type=str,
        default=None,
        metavar="LIB",
        help="run-time library (e.g. compiler-rt)",
    )
    parser.add_argument(
        "-static-libstdc++",
        dest="static_libstdcxx",
        default=False,
        action="store_true",
        help="Link with static C++ standard library",
    )
    parser.add_argument(
        "-W",
        dest="driver_flags",
        type=str,
        default=[],
        action="append",
        metavar="FLAG",
        help="Forwarded driver flags like -Wl,... -Wa,... -Wp,...",
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
        if tok.endswith(".c"):
            yield Source(file=Path(tok), dialect=SourceLanguage.C)
        if tok.endswith(".cc") or tok.endswith(".cxx") or tok.endswith(".cpp"):
            yield Source(file=Path(tok), dialect=SourceLanguage.CXX)
        if tok.endswith(".s") or tok.endswith(".S"):
            yield Source(file=Path(tok), dialect=SourceLanguage.ASM)
        if tok.endswith(".mm"):
            yield Source(file=Path(tok), dialect=SourceLanguage.OBJC)


def _link_direct_inputs(command: Iterable[str]) -> Iterable[Path]:
    for tok in command:
        if any(
            tok.endswith(ext) for ext in (".o", ".obj", ".so", ".a", ".dylib")
        ):
            yield Path(tok)


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
        if "clang" in tok:  # matches clang++
            return CompilerTool(tool=Path(tok), type=Compiler.CLANG)
        # 'g++' matches 'clang++', so this clause must come second:
        if "gcc" in tok or "g++" in tok:
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
        action="append",
        default=[],
        help="preprocessing defines",
    )
    parser.add_argument(
        "-I",
        type=Path,
        dest="includes",
        action="append",
        default=[],
        metavar="DIR",
        help="preprocessing include paths",
    )
    parser.add_argument(
        "-L",
        type=Path,
        dest="libdirs",
        action="append",
        default=[],
        metavar="DIR",
        help="linking search paths",
    )
    parser.add_argument(
        "-U",
        type=str,
        dest="undefines",
        action="append",
        default=[],
        help="preprocessing undefines",
    )
    parser.add_argument(
        "-isystem",
        type=Path,
        action="append",
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
        action="append",
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


def _linker_driver_arg_parser() -> argparse.ArgumentParser:
    """Parse linker arguments that come from -Wl,"""
    parser = argparse.ArgumentParser(
        description="Parse linker flags",
        argument_default=None,
        add_help=False,
    )
    parser.add_argument(
        "-Map",  # single-dash
        dest="mapfile",
        type=Path,
        help="linker map file to write",
    )
    parser.add_argument(
        "--Map",  # double-dash
        dest="mapfile",
        type=Path,
        help="linker map file to write",
    )
    parser.add_argument(
        "--dependency-file",
        dest="depfile",
        type=Path,
        help="dependency file to write",
    )
    parser.add_argument(
        "--just-symbols",
        type=Path,
        help="Use only the symbols of this",
    )
    parser.add_argument(
        "--retain-symbols-file",
        type=Path,
        help="file that lists symbols to retain",
    )
    parser.add_argument(
        "--version-script",
        type=Path,
        help="linking version script",
    )
    return parser


_LINKER_DRIVER_PARSER = _linker_driver_arg_parser()

# These are flags that are joined with their arguments with
# no separator (no '=' or space).
_CPP_FUSED_FLAGS = {"-I", "-D", "-L", "-U", "-isystem"}


def expand_forwarded_driver_flags(args: Iterable[str]) -> Iterable[str]:
    """Split '-Wl,foo' into ['-W', 'l,foo'] for easier argparse-ing."""
    # This helps distinguish from other options that start with -W, like -Wall.
    for arg in args:
        if any(arg.startswith(prefix) for prefix in ("-Wp,", "-Wa,", "-Wl,")):
            yield "-W"
            yield arg[2:]
        else:
            yield arg


def _assign_explicit_flag_defaults(opts: Iterable[str]) -> Iterable[str]:
    """Workarounds for argparse limitations.

    argparse doesn't have a way to accept both -flto and -flto=arg
    because with nargs='?', the former will try to consume the following
    argument.  Instead we make the implicit default explicit just for
    easy parsing.

    Default values are taken from `clang --help` documentation.
    """
    for opt in opts:
        if opt == "-flto":
            yield "-flto=full"
        elif opt == "-fprofile-generate":
            yield "-fprofile-generate=."  # directory
        elif opt == "-fprofile-instr-generate":
            yield "-fprofile-instr-generate=default.profraw"  # file
        else:
            yield opt


class CxxAction(object):
    """Attributes of C/C++ (or dialect) compilation command.

    Suitable for compilers: clang, gcc
    """

    def __init__(self, command: Sequence[str]):
        self._command = command  # keep a copy of the original command

        # Expand response files before parsing any flags.
        rsp_files = []
        rsp_expanded_args = cl_utils.expand_response_files(command, rsp_files)
        self._response_files = rsp_files

        # Expand some difficult-to-parse flags like -Wl...
        argparseable_args = list(
            expand_forwarded_driver_flags(rsp_expanded_args)
        )

        # Parse the full compiler command.
        (
            self._attributes,
            remaining_args,
        ) = _CXX_COMMAND_SCANNER.parse_known_args(
            _assign_explicit_flag_defaults(argparseable_args)
        )
        self._compiler = _find_compiler_from_command(argparseable_args)
        self._sources = list(_compile_action_sources(remaining_args))
        self._dialect = _infer_dialect_from_sources(self._sources)
        self._linker_inputs = list(_link_direct_inputs(remaining_args))

        # Handle -W driver flags.
        self._preprocessor_driver_flags = []
        self._assembler_driver_flags = []
        self._linker_driver_flags = []
        for driver_flag in self._attributes.driver_flags:
            forwarded_flags = driver_flag[2:].split(",")
            if driver_flag.startswith("p,"):
                self._preprocessor_driver_flags.extend(forwarded_flags)
            elif driver_flag.startswith("a,"):
                self._assembler_driver_flags.extend(forwarded_flags)
            elif driver_flag.startswith("l,"):
                self._linker_driver_flags.extend(forwarded_flags)

        # Linker driver flags may contain response files, so expand them.
        linker_rsp_files = []
        expanded_linker_driver_flags = cl_utils.expand_response_files(
            self.linker_driver_flags, linker_rsp_files
        )
        self._linker_response_files = linker_rsp_files

        (
            self._linker_attributes,
            unused_linker_args,
        ) = _LINKER_DRIVER_PARSER.parse_known_args(expanded_linker_driver_flags)

        # Infer the compiler command that would be used if preprocessing
        # were done separately.
        # Note: this version does not expand response files first,
        # to minimize differences relative to the original command.
        command_after_preprocessing = cl_utils.expand_fused_flags(
            self._command, _CPP_FUSED_FLAGS
        )
        (
            self._cpp_attributes,
            self._command_without_cpp_options,
        ) = _C_PREPROCESS_ARG_PARSER.parse_known_args(
            command_after_preprocessing
        )

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
    def sysroot(self) -> Optional[Path]:
        return self._attributes.sysroot

    @property
    def sources(self) -> Sequence[Source]:
        return self._sources

    @property
    def shared(self) -> bool:
        return self._attributes.shared

    @property
    def libdirs(self) -> Sequence[Path]:
        return self._attributes.libdirs

    @property
    def clang_linker_executable(self) -> str:  # basename only
        use_ld = self.use_ld or "lld"
        if "windows" in self.target:
            # Targeting Windows uses lld-link.
            if use_ld != "lld":
                raise ValueError(
                    f"Unexpected linker selected for Windows.  Expected: lld, but got: {use_ld}"
                )
            return use_ld + "-link"
        else:
            # For ld.bfd, ld.gold, ld.lld
            return "ld." + use_ld

    @property
    def linker_inputs(self) -> Sequence[Path]:
        return self._linker_inputs

    def linker_inputs_from_flags(self) -> Iterable[Path]:
        if self.linker_retain_symbols_file:
            yield self.linker_retain_symbols_file
        if self.linker_version_script:
            yield self.linker_version_script
        if self.linker_just_symbols:
            yield self.linker_just_symbols

    @property
    def response_files(self) -> Sequence[Path]:
        return self._response_files

    @property
    def linker_response_files(self) -> Sequence[Path]:
        # TODO(b/307418630): Remove this workaround after bug is fixed.
        return self._linker_response_files

    @property
    def preprocessor_driver_flags(self) -> Sequence[str]:
        return self._preprocessor_driver_flags

    @property
    def assembler_driver_flags(self) -> Sequence[str]:
        return self._assembler_driver_flags

    @property
    def linker_driver_flags(self) -> Sequence[str]:
        return self._linker_driver_flags

    @property
    def linker_depfile(self) -> Optional[Path]:
        return self._linker_attributes.depfile

    @property
    def linker_mapfile(self) -> Optional[Path]:
        return self._linker_attributes.mapfile

    @property
    def linker_just_symbols(self) -> Optional[Path]:
        return self._linker_attributes.just_symbols

    @property
    def linker_retain_symbols_file(self) -> Optional[Path]:
        return self._linker_attributes.retain_symbols_file

    @property
    def linker_version_script(self) -> Optional[Path]:
        return self._linker_attributes.version_script

    @property
    def rtlib(self) -> Optional[str]:
        return self._attributes.rtlib

    @property
    def static_libstdcxx(self) -> bool:
        return self._attributes.static_libstdcxx

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
    def sanitizers(self) -> AbstractSet[str]:
        return self._attributes.sanitize

    @property
    def using_asan(self) -> bool:
        return "address" in self._attributes.sanitize

    @property
    def using_ubsan(self) -> bool:
        return "undefined" in self._attributes.sanitize

    @property
    def profile_list(self) -> Optional[Path]:
        return self._attributes.profile_list

    @property
    def profile_generate(self) -> Optional[Path]:
        return self._attributes.profile_generate

    @property
    def profile_instr_generate(self) -> Optional[Path]:
        return self._attributes.profile_instr_generate

    @property
    def any_profile(self) -> bool:
        return self.profile_generate or self.profile_instr_generate

    @property
    def lto(self) -> Optional[str]:
        return self._attributes.lto

    @property
    def use_ld(self) -> Optional[str]:
        return self._attributes.use_ld

    @property
    def unwindlib(self) -> Optional[str]:
        return self._attributes.unwindlib

    @property
    def preprocessed_suffix(self) -> str:
        if self._dialect == SourceLanguage.CXX:
            return ".ii"
        else:
            return ".i"

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
        return Path(_remove_suffix(name, "".join(self.output_file.suffixes)))

    @property
    def uses_macos_sdk(self) -> bool:
        return str(self.sysroot).startswith("/Library/Developer/")

    # TODO: scan command for absolute paths (C++-specific)

    def input_files(self) -> Iterable[Path]:
        """Files known to be inputs based on flags."""
        # Note: reclient already infers many C++ inputs in its own
        # input processor, so it is not necessary to list them
        # for remote actions (but it does not hurt).
        for s in self.sources:
            yield s.file

        # Support for -fprofile-list was added to re-client in b/220028444.
        if self.profile_list:
            yield self.profile_list

    def output_files(self) -> Iterable[Path]:
        # Note: reclient already infers many C++ outputs in its own
        # input processor, so it is not necessary to list them
        # for remote actions (but it does not hurt).
        if self.output_file:
            yield self.output_file  # This should be first, for naming purposes

            # TODO(b/302613832): remove this once upstream supports it
            if self.save_temps:
                stem = self.save_temps_output_stem
                for suffix in (self.preprocessed_suffix, ".bc", ".s"):
                    yield stem.with_suffix(suffix)

    def output_dirs(self) -> Iterable[Path]:
        # TODO(b/272865494): remove this once upstream supports it
        if self.crash_diagnostics_dir:
            yield self.crash_diagnostics_dir

    @property
    def pdb(self) -> Optional[Path]:
        if "windows" in self.target:
            if any("/debug" in f for f in self.linker_driver_flags):
                if self.output_file:
                    return self.output_file.with_suffix(".pdb")
        return None

    def linker_output_files(self) -> Iterable[Path]:
        if self.output_file:
            yield self.output_file
        if self.linker_depfile:
            yield self.linker_depfile
        if self.linker_mapfile:
            yield self.linker_mapfile
        if self.pdb:
            yield self.pdb

    def _preprocess_only_command(self) -> Iterable[str]:
        # replace the output with a preprocessor output
        for tok in self._command:
            if tok == str(self.output_file):
                yield str(self.preprocessed_output)
            elif tok == "--save-temps":  # no need during preprocessing
                pass
            else:
                # TODO: discard irrelevant flags, like linker flags
                yield tok

        # -E tells the compiler to stop after preprocessing
        yield "-E"

        # -fno-blocks works around in issue where preprocessing includes
        # blocks-featured code when it is not wanted.
        if self.compiler_is_clang:
            yield "-fno-blocks"

    def _compile_with_preprocessed_input_command(self) -> Iterable[str]:
        # replace the first named source file with the preprocessed output
        used_preprocessed_input = False
        for tok in self._command_without_cpp_options:
            if (
                tok.endswith(".c")
                or tok.endswith(".cc")
                or tok.endswith(".cxx")
                or tok.endswith(".cpp")
            ):
                if (
                    used_preprocessed_input
                ):  # ignore other sources after the first
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
