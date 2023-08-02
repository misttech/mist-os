# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Custom argument actions for argparse."""

import argparse
import typing


class SelectionAction(argparse.Action):
    """Support appending selections to a single list in argparse.

    This action stores any value for the options in the destination.
    If multiple option strings are provided, use the longest as the canonical version.

    Example:
        parser.add_argument('-a', '--and', action=SelectionAction, dest='selection')
        parser.add_argument('selection', action=SelectionAction, dest='selection')
        assert (
            parser.parse_args(['value', '-a', 'other', '--and', 'another']).selection ==
            ['value', '--and', 'other', '--and', 'another']
        )
    """

    def __init__(
            self,
            option_strings: typing.List[str],
            dest: str,
            nargs=None,
            **kwargs):
        """Create a SelectionAction.

        Args:
            option_strings (typing.List[str]): List of options. See argparse documentation.
            dest (str): Destination variable. See argparse documentation.
            nargs (Optional[Union[int, str]]): Number of arguments. See argparse documentation.
        """

        self._dest = dest
        # When constructing a representative list of selections,
        # use the longest option as the canonical name.
        self._canonical = (
            [max(map(lambda x: (len(x), x), option_strings))[1]]
            if option_strings else [])
        if nargs is None:
            nargs = "*"
        super().__init__(list(option_strings), dest, nargs=nargs, **kwargs)

    def __call__(self, parser, namespace, values, option_string):
        """Call this parser.

        See argparse documentation for details.
        """

        if getattr(namespace, self._dest) is None:
            setattr(namespace, self._dest, [])
        lst: typing.List[str] = getattr(namespace, self._dest)
        lst += self._canonical + values
