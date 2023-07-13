# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import unittest

from util import arg_option


class TestArgOptions(unittest.TestCase):
    def test_boolean_optional_action(self):
        """Test BooleanOptionalAction.

        This test ensures that a flag --build is set to True when --build
        is passed, False when --no-build is passed, and None if not
        specified.
        """

        parser = argparse.ArgumentParser()
        parser.add_argument(
            "--build", action=arg_option.BooleanOptionalAction, default=None
        )

        self.assertEqual(parser.parse_args([]).build, None)
        self.assertEqual(parser.parse_args(["--build"]).build, True)
        self.assertEqual(parser.parse_args(["--no-build"]).build, False)

    def test_boolean_optional_action_failure(self):
        """Test that setting nargs raises an error."""

        parser = argparse.ArgumentParser()
        self.assertRaises(
            ValueError,
            lambda: parser.add_argument(
                "--foo", action=arg_option.BooleanOptionalAction, nargs=1
            ),
        )

    def test_selection_action(self):
        """Test SelectionAction.

        This test ensures that multiple arguments can all write to the
        same destination variable. Short/long names for flags are
        canonicalized to the long version.
        """

        parser = argparse.ArgumentParser()
        parser.add_argument(
            "-m", "--main-option", action=arg_option.SelectionAction, dest="option"
        )
        parser.add_argument(
            "-a", "--alt-option", action=arg_option.SelectionAction, dest="option"
        )
        parser.add_argument("option", action=arg_option.SelectionAction)

        args = parser.parse_args(["-m", "one", "two", "-a", "three", "four"])
        self.assertListEqual(
            args.option,
            [
                "--main-option",
                "one",
                "two",
                "--alt-option",
                "three",
                "four",
            ],
        )


if __name__ == "__main__":
    unittest.main()
