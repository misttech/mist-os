# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.


import os
import sys

# Fix issue with coverage finding termsim library.
sys.path.insert(0, os.path.dirname(__file__))

import contextlib
import unittest

import termsim

import termout


class TestTermout(unittest.TestCase):
    def test_single_line_overwrite(self):
        """Test that we can overwrite the contents of a single line."""
        size = termout.Size(25, 25)
        termout.reset()
        terminal = termsim.Terminal(size.columns)
        with contextlib.redirect_stdout(terminal):  # type:ignore
            termout.write_lines(["Hello"], size=size)
            self.assertListEqual(terminal.lines, ["Hello"])
            termout.write_lines(["World"], size=size)
            self.assertListEqual(terminal.lines, ["World"])

    def test_single_line_cannot_overflow(self):
        """Test that single lines are truncated to the window width."""
        size = termout.Size(3, 25)
        termout.reset()
        terminal = termsim.Terminal(size.columns)
        with contextlib.redirect_stdout(terminal):  # type:ignore
            termout.write_lines(["Hello"], size=size)
            self.assertListEqual(terminal.lines, ["Hel"])
            termout.write_lines(["World"], size=size)
            self.assertListEqual(terminal.lines, ["Wor"])

    def test_multiple_line_overwrite(self):
        """Test that we can update multiple lines"""
        size = termout.Size(25, 25)
        termout.reset()
        terminal = termsim.Terminal(size.columns)
        with contextlib.redirect_stdout(terminal):  # type:ignore
            termout.write_lines(["Hello", "World"], size=size)
            self.assertListEqual(terminal.lines, ["Hello", "World"])
            termout.write_lines(["Hello 2", "Different"], size=size)
            self.assertListEqual(terminal.lines, ["Hello 2", "Different"])

    def test_overwrite_with_different_counts(self):
        """Test that we can change the number of lines displayed"""
        size = termout.Size(25, 25)
        termout.reset()
        terminal = termsim.Terminal(size.columns)
        with contextlib.redirect_stdout(terminal):  # type:ignore
            termout.write_lines(["Hello"], size=size)
            self.assertListEqual(terminal.lines, ["Hello"])
            termout.write_lines(["Hello", "World"], size=size)
            self.assertListEqual(terminal.lines, ["Hello", "World"])
            termout.write_lines(["Hello 2"], size=size)
            # An extra line is left, which is expected.
            self.assertListEqual(terminal.lines, ["Hello 2", ""])

    def test_with_prepending_lines(self):
        """Test that we can keep a status view and prepend info lines"""
        size = termout.Size(25, 25)
        termout.reset()
        terminal = termsim.Terminal(size.columns)
        with contextlib.redirect_stdout(terminal):  # type:ignore
            termout.write_lines(["Hello"], size=size)
            self.assertListEqual(terminal.lines, ["Hello"])
            termout.write_lines(
                ["Hello", "World"],
                size=size,
                prepend=["This is a really long line that will get split."],
            )
            self.assertListEqual(
                terminal.lines,
                [
                    "This is a really long lin",
                    "e that will get split.",
                    "Hello",
                    "World",
                ],
            )
            termout.write_lines(
                ["Hello 2"], size=size, prepend=["Another", "Two", "Lines"]
            )
            # No extra line, since we appended we were able to overwrite it.
            self.assertListEqual(
                terminal.lines,
                [
                    "This is a really long lin",
                    "e that will get split.",
                    "Another",
                    "Two",
                    "Lines",
                    "Hello 2",
                ],
            )
