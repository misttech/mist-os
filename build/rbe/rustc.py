#!/usr/bin/env fuchsia-vendored-python
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""C++ compilation commands and attributes.
"""

import argparse
import collections
import dataclasses
import enum
import os

import cl_utils

from pathlib import Path
from typing import AbstractSet, Dict, Iterable, Optional, Sequence, Tuple

_RUSTC_FUSED_FLAGS = {'-C', '-L', '-Z'}


def _remove_prefix(base: str, prefix: str) -> str:
    # str.removeprefix is only available in Python 3.9+
    if base.startswith(prefix):
        return base[len(prefix):]
    return base


def _remove_suffix(base: str, suffix: str) -> str:
    # str.removesuffix is only available in Python 3.9+
    if base.endswith(suffix):
        return base[:-len(suffix)]
    return base


def _rustc_command_scanner() -> argparse.ArgumentParser:
    """Analyze rustc command attributes.

    Flags should already be canonicalized through `expand_fused_flags()`.
    """
    parser = argparse.ArgumentParser(
        description="Detects Rust compilation attributes",
        argument_default=None,
        add_help=False,
    )
    parser.add_argument(
        "-o",  # required
        type=Path,
        dest="output",
        default=None,
        metavar="FILE",
        help="compiler output",
    )
    parser.add_argument(
        "--crate-type",
        type=str,
        default=None,
        metavar="TYPE",
        help="Rust compiler output crate type",
    )
    parser.add_argument(
        "--sysroot",
        type=Path,
        default=None,
        help="compiler sysroot, where target-specific rustlibs can be found",
    )
    parser.add_argument(
        "--target",
        type=str,
        default=None,
        help="target platform",
    )
    parser.add_argument(
        "--emit",
        action='append',
        default=[],
        help=
        "Types of outputs to produce.  Values can be 'key' or 'key=value' form.",
    )
    parser.add_argument(
        "--extern",
        action='append',
        default=[],
        metavar="LIB:PATH",
        help="Specify where transitive dependencies can be found",
    )
    for f in _RUSTC_FUSED_FLAGS:
        parser.add_argument(
            f,
            type=str,
            action='append',
            dest="{}_flags".format(f.lstrip('-')),
            default=[],
            help=f"All {f}* flags",
        )
    return parser


_RUSTC_COMMAND_SCANNER = _rustc_command_scanner()


class CrateType(enum.Enum):
    UNKNOWN = 0
    RLIB = 1  # "rlib"
    BINARY = 2  # "bin"
    CDYLIB = 3  # "cdylib"
    DYLIB = 4  # "dylib"
    PROC_MACRO = 5  # "proc-macro"
    STATICLIB = 6  # "staticlib"


def parse_crate_type(crate_type) -> CrateType:
    return {
        "rlib": CrateType.RLIB,
        "bin": CrateType.BINARY,
        "cdylib": CrateType.CDYLIB,
        "dylib": CrateType.DYLIB,
        "proc-macro": CrateType.PROC_MACRO,
        "staticlib": CrateType.STATICLIB,
    }.get(crate_type, CrateType.UNKNOWN)


class InputType(enum.Enum):
    SOURCE = 0
    LINKABLE = 1  # Any of: .a, .so, .dylib


@dataclasses.dataclass
class RustcInput(object):
    file: Path
    type: InputType


def is_linkable(f: str) -> bool:
    # f might not be a Path
    return any(
        f.endswith(suffix)
        for suffix in ('.a', '.o', '.so', '.dylib', '.so.debug', '.ld'))


def find_direct_inputs(command: Iterable[str]) -> Iterable[RustcInput]:
    for tok in command:
        if tok.endswith('.rs'):
            yield RustcInput(file=Path(tok), type=InputType.SOURCE)
        elif is_linkable(tok):
            yield RustcInput(file=Path(tok), type=InputType.LINKABLE)


def find_compiler_from_command(command: Iterable[str]) -> Path:
    for tok in command:
        if 'rustc' in tok:
            return Path(tok)
    return None  # or raise error


class RustAction(object):
    """This is a rustc compile action."""

    def __init__(self, command: Sequence[str], working_dir: Path = None):
        # keep a copy of the original command
        self._original_command = command
        self._working_dir = (working_dir or Path(os.curdir)).absolute()

        # expand response files
        rsp_files = []
        self._rsp_expanded_command = list(
            cl_utils.expand_response_files(self.original_command, rsp_files))
        self._response_files = set(rsp_files)

        # analyze response-file-expanded command using canonical expanded-flag form
        self._attributes, remaining_args = _RUSTC_COMMAND_SCANNER.parse_known_args(
            list(
                cl_utils.expand_fused_flags(
                    self.rsp_expanded_command, _RUSTC_FUSED_FLAGS)))
        self._compiler = find_compiler_from_command(remaining_args)
        self._env = [
            tok for tok in
            remaining_args[:remaining_args.index(str(self._compiler))]
            if '=' in tok
        ]
        self._crate_type = parse_crate_type(self._attributes.crate_type)
        self._direct_inputs = list(find_direct_inputs(remaining_args))
        self._C_flags = cl_utils.keyed_flags_to_values_dict(
            self._attributes.C_flags)
        self._Z_flags = cl_utils.keyed_flags_to_values_dict(
            self._attributes.Z_flags)
        self._L_flags = cl_utils.keyed_flags_to_values_dict(
            self._attributes.L_flags)
        self._emit = cl_utils.keyed_flags_to_values_dict(
            cl_utils.flatten_comma_list(self._attributes.emit))
        raw_extern = cl_utils.keyed_flags_to_values_dict(
            cl_utils.flatten_comma_list(self._attributes.extern),
            convert_type=Path)
        self._extern: Dict[str, Path] = {
            k: v[-1]  # last value wins
            for k, v in raw_extern.items()
            if v  # ignore empty lists
        }

        # post-process some flags
        self._c_sysroot: Path = None
        self._use_ld: Path = None
        self._want_sysroot_libgcc = False
        self._link_arg_files: Sequence[Path] = []
        for arg in self._link_arg_flags:
            if arg == '-lgcc':
                self._want_sysroot_libgcc = True
                continue
            left, sep, right = arg.partition('=')
            if left == '--sysroot':
                self._c_sysroot = Path(right)
                continue
            if left == '-fuse-ld':
                self._use_ld = Path(right)
                continue
            if is_linkable(arg):
                self._link_arg_files.append(arg)

    @property
    def env(self) -> Sequence[str]:
        return self._env

    @property
    def original_command(self) -> Sequence[str]:
        return self._original_command

    @property
    def rsp_expanded_command(self) -> Sequence[str]:
        return self._rsp_expanded_command

    @property
    def working_dir(self) -> Path:
        return self._working_dir

    @property
    def output_file(self) -> Optional[Path]:
        return self._attributes.output  # usually this is the -o file

    @property
    def compiler(self) -> Path:
        return self._compiler

    @property
    def crate_type(self) -> CrateType:
        return self._crate_type

    @property
    def needs_linker(self) -> bool:
        return self.crate_type in {
            CrateType.BINARY, CrateType.PROC_MACRO, CrateType.DYLIB,
            CrateType.CDYLIB
        }

    @property
    def main_output_is_executable(self) -> bool:
        return self.crate_type in {
            CrateType.BINARY, CrateType.DYLIB, CrateType.CDYLIB
        }

    @property
    def want_sysroot_libgcc(self) -> bool:
        return self._want_sysroot_libgcc

    @property
    def target(self) -> Optional[str]:
        return self._attributes.target

    @property
    def direct_sources(self) -> Sequence[Path]:
        return [
            s.file for s in self._direct_inputs if s.type == InputType.SOURCE
        ]

    @property
    def response_files(self) -> AbstractSet[Path]:
        return self._response_files

    @property
    def emit(self) -> Dict[str, str]:
        return self._emit

    @property
    def emit_llvm_ir(self) -> bool:
        return 'llvm-ir' in self.emit

    @property
    def emit_llvm_bc(self) -> bool:
        return 'llvm-bc' in self.emit

    @property
    def save_analysis(self) -> bool:
        return cl_utils.last_value_of_dict_flag(
            self._Z_flags, 'save-analysis', 'no') == 'yes'

    @property
    def llvm_time_trace(self) -> bool:
        return 'llvm-time-trace' in self._Z_flags

    @property
    def extra_filename(self) -> str:
        return cl_utils.last_value_of_dict_flag(self._C_flags, 'extra-filename')

    @property
    def depfile(self) -> Optional[Path]:
        d = cl_utils.last_value_of_dict_flag(self.emit, 'dep-info', '')
        return Path(d) if d else None

    @property
    def use_ld(self) -> Optional[Path]:
        return self._use_ld

    @property
    def linker(self) -> Optional[Path]:
        d = cl_utils.last_value_of_dict_flag(self._C_flags, 'linker')
        return Path(d) if d else None

    @property
    def _link_arg_flags(self) -> Sequence[str]:
        return self._C_flags.get('link-arg', [])

    @property
    def link_arg_files(self) -> Sequence[Path]:
        return [Path(p) for p in self._link_arg_files]

    @property
    def link_map_output(self) -> Optional[Path]:
        # The linker can produce a .map output file.
        for arg in self._C_flags.get('link-args', []):
            if arg.startswith('--Map='):
                return Path(_remove_prefix(arg, '--Map='))
        return None

    def default_rust_sysroot(self) -> Path:
        """This is the relative location of rust sysroot, when unspecified."""
        command = [str(self.compiler), '--print', 'sysroot']
        result = cl_utils.subprocess_call(
            command, cwd=self.working_dir, quiet=True)
        if result.returncode != 0:
            raise RuntimeError('Error: unable to infer default rust sysroot')
        # expect one line with the absolute path to the sysroot
        sysroot_abs = Path(result.stdout[0].strip())
        sysroot_rel = cl_utils.relpath(sysroot_abs, start=self.working_dir)
        return sysroot_rel

    @property
    def rust_sysroot(self) -> Path:
        """This is where the target rustlibs for all platforms live."""
        return self._attributes.sysroot or self.default_rust_sysroot()

    @property
    def c_sysroot(self) -> Optional[Path]:
        return self._c_sysroot

    @property
    def native(self) -> Sequence[Path]:
        return [Path(p) for p in self._L_flags.get('native', [])]

    @property
    def native_link_arg_files(self) -> Iterable[Path]:
        for path in self.native:
            if path.is_dir():
                # TODO: debug print
                pass
            elif path.is_file():
                yield path
                # caller might need to prepend $build_dir

    @property
    def explicit_link_arg_files(self) -> Sequence[Path]:
        return [
            s.file for s in self._direct_inputs if s.type == InputType.LINKABLE
        ]

    @property
    def externs(self) -> Dict[str, Path]:
        return self._extern

    def extern_paths(self) -> Iterable[Path]:
        yield from self.externs.values()

    @property
    def _output_file_base(self) -> str:
        """Removes any .rlib or .exe suffix to get the stem name."""
        # Returning str instead of Path because caller most likely
        # wants to append something to the result to form a Path name.
        if not self.output_file:
            raise RuntimeError(
                'Cannot infer stem name without a named -o output file')
        return str(self.output_file.parent / self.output_file.stem)

    @property
    def _auxiliary_output_path(self) -> str:
        # Returning str instead of Path because caller most likely
        # wants to append something to the result to form a Path name.
        return self._output_file_base + self.extra_filename

    def extra_output_files(self) -> Iterable[Path]:
        base = self._auxiliary_output_path
        if self.emit_llvm_ir:
            yield Path(base + '.ll')
        if self.emit_llvm_bc:
            yield Path(base + '.bc')
        if self.save_analysis:
            # Path() construction already normalizes away any leading './'
            analysis_file = Path(
                'save-analysis-temp',
                Path(self._auxiliary_output_path + '.json').name)
            yield analysis_file
        if self.llvm_time_trace:
            trace_file = Path(self._output_file_base + '.llvm_timings.json')
            yield trace_file
        link_map = self.link_map_output
        if link_map:
            yield link_map

    def dep_only_command(self, depfile_name: str) -> Iterable[str]:
        """Generate a command that only produces a depfile."""
        new_emit_args = [
            f'--emit=dep-info={depfile_name}', '-Z', 'binary-dep-depinfo'
        ]
        replaced_emit = False
        # Use the response-file-expanded form, in case --emit is inside
        # a response file.
        for tok in self.rsp_expanded_command:
            if tok.startswith('--emit'):  # replace the original emit
                if replaced_emit:
                    pass
                else:
                    replaced_emit = True
                    yield from new_emit_args
            else:
                yield tok

        # if we haven't seen emit yet, add it
        if not replaced_emit:
            yield from new_emit_args


# TODO: write a main() routine just for printing debug info about a compile
# command
