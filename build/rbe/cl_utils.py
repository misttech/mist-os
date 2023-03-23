#!/usr/bin/env python3.8
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Generic utilities for working with command lines and argparse.
"""

import collections

from typing import Dict, FrozenSet, Iterable, Sequence


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
        flags: Iterable[str]) -> Dict[str, Sequence[str]]:
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

    Returns:
      Strings dictionary of key and (possibly multiple) values.
    """
    partitions = (f.partition('=') for f in flags)
    # each partition is a tuple (left, sep, right)
    d = collections.defaultdict(list)
    for (key, sep, value) in partitions:
        if sep == '=':
            d[key].append(value)
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
