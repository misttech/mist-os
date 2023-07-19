#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Expand linker inputs, handle linker scripts encountered.

Base on documentation at:
https://sourceware.org/binutils/docs-2.40/ld/Simple-Commands.html
"""

import argparse
import dataclasses
import depfile
import enum
import os
import re
import subprocess
import sys

import cl_utils

from pathlib import Path
from typing import Iterable, Optional, Sequence, Tuple, Union

_SCRIPT_BASENAME = Path(__file__).name


def msg(text: str):
    print(f'[{_SCRIPT_BASENAME}] {text}')


_VERBOSE = False


def vmsg(text: str):
    if _VERBOSE:
        msg(text)


# Set of known headers of various library file types.
# We just need the subset of headers that covers
# library files we expect to encounter.
# Source: https://en.wikipedia.org/wiki/List_of_file_signatures
LIBRARY_FILE_MAGIC_NUMBERS = {
    b'!<arch>',  # archives (.a) (on Linux, MacOS)
    b'\x7fELF',  # ELF files
    b'\xfe\xed\xfa\xce',  # Mach-O 32b
    b'\xfe\xed\xfa\xcf',  # Mach-O 64b
    b'\xce\xfa\xed\xfe',  # Mach-O 32b, reverse byte-ordering
    b'\xcf\xfa\xed\xfe',  # Mach-O 32b, reverse byte-ordering
    b'\xca\xfe\xfa\xbe',  # Mach-O Fat binary
    b'\x5a\x4d',  # MS-DOS compatible, Portable Executable (PE-COFF)
    # TODO: handle other PE cases: .LIB
}


class TokenType(enum.Enum):
    KEYWORD = 0  # e.g. INCLUDE, INPUT, GROUP, etc.
    ARG = 1
    COMMA = 2
    OPEN_PAREN = 3
    CLOSE_PAREN = 4
    SPACE = 5
    NEWLINE = 6
    COMMENT = 7


_KEYWORDS_RE = re.compile(
    r'(INCLUDE|INPUT|GROUP|AS_NEEDED|OUTPUT_FORMAT|OUTPUT|SEARCH_DIR|STARTUP|TARGET)'
)
_SPACE_RE = re.compile(r'[ \t]+')
_NEWLINE_RE = re.compile(r'\r?\n')
_COMMENT_RE = re.compile(r'/\*[^*]*\*/', re.MULTILINE)
_ARG_RE = re.compile(r'[^, \t\r\n()]+')


class LexError(ValueError):

    def __init__(self, msg: str):
        super().__init__(msg)


class ParseError(ValueError):

    def __init__(self, msg: str):
        super().__init__(msg)


@dataclasses.dataclass
class Token(object):
    text: str
    type: TokenType


def _lex_linker_script(text: str) -> Iterable[Token]:
    """Lex the full text of a linker script.

    Args:
      text: full contents of linker script

    Yields:
      linker script Tokens

    Raises:
      LexError if there are unhandled input cases, lexical errors.
    """
    while text:  # is not empty
        next_char = text[0]

        if next_char == '(':
            yield Token(text=next_char, type=TokenType.OPEN_PAREN)
            text = text[1:]
            continue

        if next_char == ')':
            yield Token(text=next_char, type=TokenType.CLOSE_PAREN)
            text = text[1:]
            continue

        if next_char == ',':
            yield Token(text=next_char, type=TokenType.COMMA)
            text = text[1:]
            continue

        keyword_match = _KEYWORDS_RE.match(text)
        if keyword_match:
            keyword_name = keyword_match.group(0)
            yield Token(text=keyword_name, type=TokenType.KEYWORD)
            text = text[len(keyword_name):]
            continue

        comment_match = _COMMENT_RE.match(text)
        if comment_match:
            comment_name = comment_match.group(0)
            yield Token(text=comment_name, type=TokenType.COMMENT)
            text = text[len(comment_name):]
            continue

        space_match = _SPACE_RE.match(text)
        if space_match:
            space_name = space_match.group(0)
            yield Token(text=space_name, type=TokenType.SPACE)
            text = text[len(space_name):]
            continue

        newtext_match = _NEWLINE_RE.match(text)
        if newtext_match:
            newtext_name = newtext_match.group(0)
            yield Token(text=newtext_name, type=TokenType.NEWLINE)
            text = text[len(newtext_name):]
            continue

        arg_match = _ARG_RE.match(text)
        if arg_match:
            arg_name = arg_match.group(0)
            yield Token(text=arg_name, type=TokenType.ARG)
            text = text[len(arg_name):]
            continue

        line_remainder = text.splitlines()[0]
        raise LexError(
            f'[linker_script.lex] Unrecognized text: "{line_remainder}"')


def _filter_tokens(toks: Iterable[Token]) -> Iterable[Token]:
    """Drop un-important tokens like spaces."""
    for tok in toks:
        if tok.type in {TokenType.KEYWORD, TokenType.ARG, TokenType.OPEN_PAREN,
                        TokenType.CLOSE_PAREN}:
            yield tok


class Directive(object):
    """Represents a single linker script directive."""

    def __init__(self, keyword: str, args: Sequence[str] = None):
        self.name = keyword  # function name
        # Some Directive arguments contain other directives,
        # e.g. AS_NEEDED inside INPUT or GROUPS.
        self.args: Sequence[Union[str, 'Directive']] = args or []

    def __str__(self):
        args_str = ' '.join(str(arg) for arg in self.args)
        return f'{self.name}({args_str})'


def _parse_directive(name: str, toks: Iterable[Token]) -> Directive:
    """Recursively parse a single linker script directive."""
    # Most directives' args are inside parentheses, but not INCLUDE
    vmsg(f'Parsing {name} directive')
    current_directive = Directive(name)

    if name == 'INCLUDE':  # special case
        include_arg = next(toks)  # expect one arg
        current_directive.args.append(include_arg)
        return current_directive

    # All other directives's args are enclosed in ().

    open_paren = next(toks)
    if open_paren.type != TokenType.OPEN_PAREN:
        raise ParseError(f"Expecting '(' but got: {open_paren.text}")

    got_close = False
    for tok in toks:
        if tok.type == TokenType.CLOSE_PAREN:
            got_close = True
            break

        if tok.type == TokenType.KEYWORD:
            current_directive.args.append(
                _parse_directive(tok.text, toks))  # recursive
            continue

        vmsg(f'Appending arg: {tok.text}')
        current_directive.args.append(tok.text)

    if not got_close:
        raise ParseError(f'Unterminated linker script directive, {name}')

    return current_directive


def _parse_linker_script_directives(
        toks: Iterable[Token]) -> Iterable[Directive]:
    while True:
        try:
            tok = next(toks)
        except StopIteration:
            break

        if tok.type != TokenType.KEYWORD:
            raise ParseError(
                f'Expected a linker script keyword, but got: {tok.text}')
        yield _parse_directive(tok.text, toks)


_LINKABLE_EXTENSIONS = ('.a', '.so', '.ld', '.dylib')


def _flatten_as_needed(arg: Union[str, Directive]) -> Iterable[str]:
    if isinstance(arg, Directive) and arg.name == 'AS_NEEDED':
        yield from arg.args
    else:  # is just a str
        yield arg


class LinkerInvocation(object):
    """Mimics a linker invocation."""
    def __init__(self,
                 working_dir_abs: Path = None,
                 search_paths: Sequence[Path] = None,
                 l_libs: Sequence[str] = None,  # e.g. "c" from "-lc"
                 direct_files: Sequence[Path] = None,
                 sysroot: Path = None,
                 ):
        working_dir_abs = working_dir_abs or Path(os.curdir).absolute()
        assert working_dir_abs.is_absolute()
        self._working_dir_abs = working_dir_abs

        self._search_paths = search_paths or []
        self._l_libs = l_libs or []
        self._direct_files = direct_files or []
        self._sysroot = sysroot

    @property
    def working_dir(self) -> Path:
        return self._working_dir_abs

    @property
    def search_paths(self) -> Sequence[Path]:
        return self._search_paths

    @property
    def l_libs(self) -> Sequence[str]:
        return self._l_libs

    @property
    def direct_files(self) -> Sequence[Path]:
        return self._direct_files

    @property
    def sysroot(self) -> Optional[Path]:
        return self._sysroot

    def expand_linker_script(self, text: Optional[str]) -> Iterable[Path]:
        if text is None:
            return
        directives = _parse_linker_script_directives(
            _filter_tokens(_lex_linker_script(text)))
        yield from self.handle_directives(directives)

    def _include(self, directive: Directive) -> Iterable[Path]:
        """Include another linker script."""
        for arg in directive.args:
            p = self.resolve_path(Path(arg.text), check_sysroot=False)
            if p:
                # Want the included script, and whatever else it points to.
                yield p
                p_abs = self.abs_path(p)
                yield from self.expand_linker_script(
                    try_linker_script_text(p_abs))

    def _input(self, directive: Directive) -> Iterable[Path]:
        """Directly use these as linker arguments.  These are not linker scripts."""
        for arg in directive.args:
            for lib in _flatten_as_needed(arg):
                if lib.startswith('-l'):
                    p = self.resolve_lib(lib[2:])
                else:
                    p = self.resolve_path(Path(lib), check_sysroot=True)
                    vmsg(f'resolved to {p}')
                if p:
                    yield p

    def _group(self, directive: Directive) -> Iterable[Path]:
        for arg in directive.args:
            for lib in _flatten_as_needed(arg):
                vmsg(f'flattened: {lib}')
                # All arguments should be archives (no -l).
                p = self.resolve_path(
                    Path(lib),
                    check_sysroot=True)  # should already include file extension
                if p:
                    yield p

    def _search_dir(self, directive: Directive) -> None:
        """Add a lib search path.  (Mutates self)"""
        for arg in directive.args:
            self._search_paths.append(Path(arg))

    def _ignore(self, directive: Directive) -> None:
        """Ignored directive."""
        pass

    def _not_implemented(self, directive: Directive) -> Iterable[Path]:
        """Known unimplemented directive."""
        raise NotImplementedError(
            f'Encountered unhandled linker script directive: {directive.name}')

    def _handle_directive(self, directive: Directive) -> Iterable[Path]:
        handler_map = {
            # Functions can yield Paths or return None
            'INCLUDE': self._include,
            'INPUT': self._input,
            'GROUP': self._group,
            'OUTPUT': self._ignore,
            'OUTPUT_FORMAT': self._ignore,
            'SEARCH_DIR': self._search_dir,
            'TARGET': self._ignore,
            # Not implemented:
            # 'STARTUP':
        }

        try:
            handler = handler_map[directive.name]
        except KeyError:
            raise NotImplementedError(
                f'Encountered unhandled linker script directive: {directive.name}'
            )

        result = handler(directive)
        if result:
            yield from result

    def handle_directives(self,
                          directives: Iterable[Directive]) -> Iterable[Path]:
        for d in directives:
            vmsg(f'Handling directive: {d}')
            yield from self._handle_directive(d)

    def abs_path(self, path: Path) -> Path:  # absolute
        """Returns an absolute path to 'path'.

        This allows us to perform existence checks using
        relative paths without having to os.chdir(),
        which is important for concurrency.
        """
        if path.is_absolute():
            return path
        return self.working_dir / path

    def path_exists(self, path: Path) -> bool:
        return self.abs_path(path).exists()

    def resolve_path(self, path: Path, check_sysroot: bool) -> Optional[Path]:
        if self.path_exists(path):
            return path

        for s in self.search_paths:
            p = s / path
            if self.path_exists(p):
                return p

        if check_sysroot and self.sysroot:
            p = self.sysroot / path
            vmsg(f'checking in sysroot {p}')
            if self.path_exists(p):
                return p

        return None

    def resolve_lib(self, lib: str) -> Optional[Path]:
        """Resolve a linker input reference, using search paths, trying various lib extensions.

        Args:
          lib: library name, like 'foo' in '-lfoo' -> 'libfoo.a'

        Returns:
          full path to 'lib{lib}.{ext}', if found, else None.
        """
        force_sysroot = False
        # Entries that start with '=' should only be searched in the sysroot.
        if lib.startswith('='):
            force_sysroot = True
            stem = 'lib' + lib[1:]
        else:
            stem = 'lib' + lib

        if not force_sysroot:
            for s in self.search_paths:
                for ext in _LINKABLE_EXTENSIONS:
                    p = s / (stem + ext)
                    if self.path_exists(p):
                        return p

        if self.sysroot:
            for ext in _LINKABLE_EXTENSIONS:
                p = self.sysroot / (stem + ext)
                if self.path_exists(p):
                    return p

        # Unable to resolve.
        return None

    def expand_all(self) -> Iterable[Path]:
        """Expands linker args, possible by examining linker scripts.

        Yields:
          paths to linker input files.
        """
        for lib in self.l_libs:
            vmsg(f'Expanding: {lib}')
            resolved = self.resolve_lib(lib)
            if resolved:
                yield from self.expand_possible_linker_script(resolved)

        for f in self.direct_files:
            vmsg(f'Expanding: {f}')
            yield from self.expand_possible_linker_script(f)

    def expand_possible_linker_script(self, lib: Path) -> Iterable[Path]:
        """Finds other files referenced if `lib` is a linker script."""
        yield lib
        # parse it and expand
        yield from self.expand_linker_script(try_linker_script_text(lib))

        # Otherwise, it is a regular linker binary file.
        # Nothing else to do.

    def expand_using_lld(self, lld: Path,
                         inputs: Sequence[Path]) -> Iterable[Path]:
        """Use lld to expand linker inputs, including linker scripts.

        Works like clang-scan-deps, but for linking.
        This is useful for preparing sets of linker inputs
        for remote building.

        Args:
          lld: path to ld.lld binary
          inputs: linker arguments: -llibs, and other linker input files.

        Yields:
          linker inputs encountered by lld.
        """
        lld_command = [
            str(lld),
            '-o',
            '/dev/null',  # Don't want link output
            '--dependency-file=/dev/stdout',  # avoid temp file
        ] + ['--sysroot={self.sysroot}'] if self.sysroot else [] + [
            '-L{path}' for path in self.search_paths
        ] + self.l_libs + [str(f) for f in self.direct_files + inputs]

        result = cl_utils.subprocess_call(
            command=lld_command, cwd=self.working_dir)

        if result.returncode != 0:
            err_msg = '\n'.join(result.stderr)
            raise RuntimeError(f'lld command failed: {lld_command}\n{err_msg}')

        # newlines are important separators
        depfile_lines = [line + '\n' for line in result.stdout]
        deps = [
            dep for dep in depfile.parse_lines(depfile_lines)
            if not dep.is_phony
        ]
        assert len(
            deps
        ) == 1, f'Expecting only one non-phony dep from lld depfile, but got {len(deps)}'
        yield from deps[0].deps_paths


def try_linker_script_text(path: Path) -> Optional[str]:
    """Returns text from linker script, or None if it is not a linker script."""
    try:
        contents = path.read_text()
    except UnicodeDecodeError:
        return None

    # It is possible for some binary formats to successfully read as text,
    # so we must check some headers of known library file formats.
    first_bytes = contents[:8].encode()
    if any(first_bytes.startswith(prefix)
           for prefix in LIBRARY_FILE_MAGIC_NUMBERS):
        # Not a linker script.
        return None

    return contents


def _main_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Expand linker args and scripts into set of files.",
        argument_default=None,
    )
    parser.add_argument(
        "--verbose",
        action="store_true",
        default=False,
        help="Show what is happening.",
    )
    parser.add_argument(
        "-L",
        dest='link_paths',
        action="append",
        help="Add a linker search path.",
    )
    parser.add_argument(
        "-l",
        dest='libs',
        action="append",
        help="Add a library (searched).",
    )
    parser.add_argument(
        "--sysroot",
        type=Path,
        default=None,
        help="Specify sysroot path.",
    )
    # Positional args are the command and arguments to run.
    parser.add_argument(
        "objects",
        type=Path,
        nargs="*",
        help="Objects, archives, libs to directly link.",
    )
    return parser


_MAIN_ARG_PARSER = _main_arg_parser()


def _main(argv: Sequence[str], working_dir_abs: Path) -> int:
    args = _MAIN_ARG_PARSER.parse_args(argv)

    global _VERBOSE
    _VERBOSE = args.verbose

    link = LinkerInvocation(
        working_dir_abs=working_dir_abs,
        search_paths=args.link_paths,
        l_libs=args.libs,
        direct_files=args.objects,
        sysroot=args.sysroot,
    )
    paths = list(link.expand_all())
    for p in paths:
        # make relative to working_dir_abs
        print(f'{p}')

    return 0


def main(argv: Sequence[str]) -> int:
    return _main(
        argv,
        working_dir_abs=Path(os.curdir).absolute(),
    )


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
