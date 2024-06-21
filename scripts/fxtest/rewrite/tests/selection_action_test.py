# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os
import tempfile
import unittest
import unittest.mock as mock
import argparse

from parameterized import parameterized
import selection_action

import args
import config


class TestSelectionAction(unittest.TestCase):
    def test_selection_action(self) -> None:
        """Test SelectionAction.

        This test ensures that multiple arguments can all write to the
        same destination variable. Short/long names for flags are
        canonicalized to the long version.
        """

        parser = argparse.ArgumentParser()
        parser.add_argument(
            "-p",
            "--package",
            action=selection_action.SelectionAction,
            nargs=0,
            dest="option",
        )
        parser.add_argument(
            "-c",
            "--component",
            action=selection_action.SelectionAction,
            nargs=0,
            dest="option",
        )
        parser.add_argument(
            "option", action=selection_action.SelectionAction, nargs="*"
        )

        args = parser.parse_intermixed_args(
            selection_action.SelectionAction.preprocess_args(
                ["-p", "one", "two", "-c", "three", "four"]
            )
        )
        self.assertListEqual(
            args.option,
            [
                "--package",
                "one",
                "two",
                "--component",
                "three",
                "four",
            ],
        )
