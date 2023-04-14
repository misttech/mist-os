#!/usr/bin/env python3.8
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Generic utilities for working with command lines and argparse.
"""

import argparse
import asyncio
import collections
import io
import os
import shlex
import sys
import platform

from pathlib import Path
from typing import Any, Callable, Dict, FrozenSet, Iterable, Sequence

# Local subprocess and remote environment calls need this when a
# command is prefixed with an X=Y environment variable.
_ENV = '/usr/bin/env'


def auto_env_prefix_command(command: Sequence[str]) -> Sequence[str]:
    if not command:
        return []
    if '=' in command[0]:
        # Commands that start with X=Y local environment variables
        # need to be run with 'env'.
        return [_ENV] + command
    return command


def bool_golang_flag(value: str) -> bool:
    """Interpret a Go-lang flag style boolean.

    See https://pkg.go.dev/flag

    This can be used as a 'type' parameter to 'ArgumentParser.add_argument()'
    """
    return {
        '1': True,
        '0': False,
        't': True,
        'f': False,
        'true': True,
        'false': False,
    }[value.lower()]


def command_quoted_str(command: Iterable[str]) -> str:
    return ' '.join(shlex.quote(t) for t in command)


def flatten_comma_list(items: Iterable[str]) -> Iterable[str]:
    """Flatten ["a,b", "c,d"] -> ["a", "b", "c", "d"].

    This is useful for merging repeatable flags, which also
    have comma-separated values.

    Yields:
      Elements that were separated by commas, flattened over
      the original sequence..
    """
    for item in items:
        yield from item.split(',')


def expand_fused_flags(command: Iterable[str],
                       flags: Sequence[str]) -> Iterable[str]:
    """Expand "fused" flags like '-I/foo/bar' into ('-I', '/foo/bar').

    argparse.ArgumentParser does not handle fused flags well,
    so expanding them first makes it easier to parse.
    Do not expect the intended tool to be able to parse these expanded flags.

    The reverse transformation is `fuse_expanded_flags()`.

    Args:
      command: sequence of command tokens.
      flags: flag prefixes that are to be separated from their values.

    Yields:
      command tokens, possibly expanded.
    """
    for tok in command:
        matched = False
        for prefix in flags:
            if tok.startswith(prefix) and len(tok) > len(prefix):
                # Separate value from flag to make it easier for argparse.
                yield prefix
                yield tok[len(prefix):]
                matched = True
                break

        if not matched:
            yield tok


def fuse_expanded_flags(command: Iterable[str],
                        flags: FrozenSet[str]) -> Iterable[str]:
    """Turns flags like ('-D' 'foo') into '-Dfoo'.

    Reverse transformation of `expand_fused_flags()`.

    Args:
      command: sequence of command tokens.
      flags: flag prefixes that are to be joined with their values.

    Yields:
      command tokens, possibly fused.
    """
    prefix = None
    for tok in command:
        if prefix:
            yield prefix + tok
            prefix = None
            continue
        if tok in flags:
            # defer to next iteration to fuse
            prefix = tok
            continue

        yield tok


def keyed_flags_to_values_dict(
        flags: Iterable[str],
        convert_type: Callable[[str], Any] = None) -> Dict[str, Sequence[str]]:
    """Convert a series of key[=value]s into a dictionary.

    All dictionary values are accumulated sequences of 'value's,
    so repeated keys like 'k=x' and 'k=y' will result in k:[x,y].
    It is up to the caller to interpret the values.

    This is useful for parsing and organizing tool flags like
    '-Cfoo=bar', '-Cbaz', '-Cquux=foo'.

    Args:
      flags: strings with the following forms:
        'key' -> key: (no value)
        'key=' -> key: "" (empty string)
        'key=value' -> key: value
      convert_type: type to convert string to, e.g. int, Path

    Returns:
      Strings dictionary of key and (possibly multiple) values.
    """
    partitions = (f.partition('=') for f in flags)
    # each partition is a tuple (left, sep, right)
    d = collections.defaultdict(list)
    for (key, sep, value) in partitions:
        if sep == '=':
            d[key].append(convert_type(value) if convert_type else value)
        else:
            d[key]
    return d


def last_value_or_default(values: Sequence[str], default: str) -> str:
    if values:
        return values[-1]
    return default


def last_value_of_dict_flag(
        d: Dict[str, Sequence[str]], key: str, default: str = '') -> str:
    """This selects the last value among repeated occurrences of a flag as a winner."""
    return last_value_or_default(d.get(key, []), default)


def relpath(path: Path, start: Path) -> Path:
    """Relative path (using Path objects).

    Path.relative_to() requires self to be a subpath of the argument,
    but here, the argument is often the subpath of self.
    Hence, we need os.path.relpath() in the general case.

    Args:
      path (Path): target path
      start (Path): starting directory (required, unlike os.path.relpath)

    Returns:
      relative path
    """
    return Path(os.path.relpath(path, start=start))


def symlink_relative(dest: Path, src: Path):
    """Create a relative-path symlink from src to dest.

    Like os.symlink(), but using relative path.
    Any intermediate directories to src are automatically created.

    Args:
      dest: target to link-to (not required to exist)
      src: new symlink path pointing to dest
    """
    src.parent.mkdir(parents=True, exist_ok=True)
    src.symlink_to(relpath(dest, start=src.parent))

#####################################################################
# The following code implements subprocess 'tee' behavior based on:
# https://stackoverflow.com/questions/2996887/how-to-replicate-tee-behavior-in-python-when-using-subprocess

class SubprocessResult(object):

    def __init__(self,
                 returncode: int,
                 stdout: Sequence[str] = None,  # lines
                 stderr: Sequence[str] = None,  # lines
                 # The process id may come in handy when looking for logs
                 pid: int = None,
                 ):
        self.returncode = returncode
        self.stdout = stdout or []
        self.stderr = stderr or []
        self.pid = pid if pid is not None else -1


async def _read_stream(stream: io.TextIOBase, callback: Callable[[str], None]):
    while True:
        line = await stream.readline()
        if line:
            callback(line)
        else:
            break


async def _stream_subprocess(
    cmd: Sequence[str],
    stdin: io.TextIOBase = None,
    stdout: io.TextIOBase = None,
    stderr: io.TextIOBase = None,
    quiet: bool = False,
    **kwargs,
) -> SubprocessResult:
    popen_kwargs = {}
    if platform.system() == 'Windows':
        platform_settings = {"env": os.environ}
    else:
        platform_settings = {}
        # default interpreter is sufficient: {"executable": "/bin/sh"}

    popen_kwargs.update(platform_settings)
    popen_kwargs.update(kwargs)

    cmd_str = command_quoted_str(cmd)
    p = await asyncio.create_subprocess_shell(
        cmd_str,
        stdin=stdin,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
        **popen_kwargs)
    pid = p.pid

    out_text = []
    err_text = []

    def tee(line: str, sink: Sequence[str], pipe: io.TextIOBase):
        line = line.decode("utf-8").rstrip()
        sink.append(line)
        if not quiet:
            print(line, file=pipe)

    await asyncio.wait(
        [   # Forward stdout, stderr while capturing them.
            _read_stream(p.stdout, lambda l: tee(l, out_text, stdout or sys.stdout)),
            _read_stream(p.stderr, lambda l: tee(l, err_text, stderr or sys.stderr)),
        ])

    return SubprocessResult(
        returncode=await p.wait(),
        stdout=out_text,
        stderr=err_text,
        pid=pid,
    )


def subprocess_call(
    cmd: Sequence[str],
    stdin: io.TextIOBase = None,
    stdout: io.TextIOBase = None,
    stderr: io.TextIOBase = None,
    quiet: bool = False,
    **kwargs,
) -> SubprocessResult:
    """Similar to subprocess.call(), but records stdout/stderr.

    Use this when interested in stdout/stderr.

    Args:
      cmd: command to execute
      stdin: input stream
      stdout: output stream
      stderr: error stream
      quiet: if True, suppress forwarding to sys.stdout/stderr.
      **kwargs: forwarded subprocess.Popen arguments.

    Returns:
      returncode, stdout (text), stderr (text)
    """
    loop = asyncio.get_event_loop()
    result = loop.run_until_complete(
        _stream_subprocess(
            cmd=cmd,
            stdin=stdin,
            stdout=stdout,
            stderr=stderr,
            quiet=quiet,
            **kwargs,
        ))
    return result

# end of subprocess_call section
#####################################################################
