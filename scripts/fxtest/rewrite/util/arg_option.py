# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Custom argument actions for argparse."""

import argparse
import typing


class BooleanOptionalAction(argparse.Action):
    """Boolean action supporting "--no-" prefix for argparse.

    Implemented as argparse.BooleanOptionalAction in Python 3.9, this definition
    allows us to support Python 3.8.

    TODO(b/291123226): Remove this when Python 3.9 is supported.

    Example:
        parser.add_argument('--something', action=BooleanOptionalAction)
        assert parser.parse_args(['--something']).something == True
        assert parser.parse_args(['--no-something']).something == False

    """

    def __init__(
        self,
        option_strings: typing.List[str],
        dest: str,
        nargs: typing.Optional[typing.Union[str, int]] = None,
        **kwargs,
    ) -> None:
        """
        Args:
            option_strings (typing.List[str]): List of options. See argparse documentation.
            dest (str): Destination variable. See argparse documentation.
            nargs (Optional[Union[int, str]]): Number of arguments. See argparse documentation.
        """
        if nargs is not None:
            raise ValueError("nargs is not allowed")

        self._option_strings: typing.List[str] = option_strings.copy()
        for s in option_strings:
            if s.find("--") == 0:
                self._option_strings.append(f"--no{s[1:]}")

        super().__init__(list(self._option_strings), dest, nargs=0, **kwargs)

    def __call__(self, parser, namespace, values, option_string):
        """Call this parser.

        See argparse documentation for details.
        """

        if option_string in self._option_strings:
            setattr(namespace, self.dest, option_string.find("--no-") != 0)


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
        self, option_strings: typing.List[str], dest: str, nargs=None, **kwargs
    ):
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
            if option_strings
            else []
        )
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
