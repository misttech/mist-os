#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import unittest

import verify_product_configuration


def fake_file_sha1(path: str) -> str:
    return "abcdef"


verify_product_configuration.file_sha1 = fake_file_sha1


class VerifyProductConfigurations(unittest.TestCase):
    def test_sha_file(self) -> None:
        dict_in = {
            "one": "path/to/file.txt",
        }
        expected = {
            "one_sha1": "abcdef",
        }
        verify_product_configuration.normalize_file_in_config(
            dict_in, "one", "root"
        )
        self.assertEqual(expected, dict_in)

    def test_sha_nested_file(self) -> None:
        dict_in = {
            "one": {
                "two": {
                    "three": "path/to/file.txt",
                },
            },
        }
        expected = {
            "one": {
                "two": {
                    "three_sha1": "abcdef",
                },
            },
        }
        verify_product_configuration.normalize_file_in_config(
            dict_in, "one.two.three", "root"
        )
        self.assertEqual(expected, dict_in)

    def test_sha_nested_list(self) -> None:
        dict_in = {
            "one": {
                "two": {
                    "three": ["path/to/file.txt", "path/to/other/file.txt"],
                },
            },
        }
        expected = {
            "one": {
                "two": {
                    "three_sha1": ["abcdef", "abcdef"],
                },
            },
        }
        verify_product_configuration.normalize_file_in_config(
            dict_in, "one.two.three[]", "root"
        )
        self.assertEqual(expected, dict_in)

    def test_sha_nested_files_under_list(self) -> None:
        dict_in = {
            "one": {
                "two": [
                    {
                        "three": "path/to/file.txt",
                    },
                ],
            },
        }
        expected = {
            "one": {
                "two": [
                    {
                        "three_sha1": "abcdef",
                    },
                ],
            },
        }
        verify_product_configuration.normalize_file_in_config(
            dict_in, "one.two[].three", "root"
        )
        self.assertEqual(expected, dict_in)

    def test_sha_nested_list_under_list(self) -> None:
        dict_in = {
            "one": {
                "two": [
                    {
                        "three": ["path/to/file.txt", "path/to/other/file.txt"],
                    },
                ],
            },
        }
        expected = {
            "one": {
                "two": [
                    {
                        "three_sha1": ["abcdef", "abcdef"],
                    },
                ],
            },
        }
        verify_product_configuration.normalize_file_in_config(
            dict_in, "one.two[].three[]", "root"
        )
        self.assertEqual(expected, dict_in)
