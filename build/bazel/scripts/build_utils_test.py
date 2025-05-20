#!/usr/bin/env fuchsia-vendored-python
# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

_SCRIPT_DIR = os.path.dirname(__file__)
sys.path.insert(0, _SCRIPT_DIR)
import build_utils
from build_utils import BazelCommandResult, BazelLauncher, BazelQueryCache


class IsHexadecimalStringTest(unittest.TestCase):
    def test_is_hexadecimal_string(self) -> None:
        TEST_CASES = [
            ("", False),
            ("0", True),
            ("0.", False),
            ("F", True),
            ("G", False),
            ("0123456789abcdefABCDEF", True),
            ("0123456789abcdefghijklmnopqrstuvwxyz", False),
        ]
        for input, expected in TEST_CASES:
            self.assertEqual(
                build_utils.is_hexadecimal_string(input),
                expected,
                msg=f"For input [{input}]",
            )


class IsLikelyBuildIdPathTest(unittest.TestCase):
    def test_one(self) -> None:
        TEST_CASES = [
            ("", False),
            ("/src/.build-id", False),
            ("/src/.build-id/0", False),
            ("/src/.build-id/00", False),
            ("/src/.build-id/000", False),
            ("/src/.build-id/0/foo", False),
            ("/src/.build-id/00/foo", True),
            ("/src/.build-id/000/foo", False),
            ("/src/.build-id/af/foo", True),
            ("/src/.build-id/ag/foo", False),
            ("/src/..build-id/00/foo", False),
            ("/src.build-id/00/foo", False),
            ("/src/.build-id/log.txt", False),
            (".build-id/00/foo", True),
        ]
        for input, expected in TEST_CASES:
            self.assertEqual(
                build_utils.is_likely_build_id_path(input),
                expected,
                msg=f"For input [{input}]",
            )


class IsLikelyContentHashPath(unittest.TestCase):
    def test_one(self) -> None:
        TEST_CASES = [
            ("", False),
            ("/src/.build-id/0/foo", False),
            ("/src/.build-id/00/foo", True),
            ("/src/blobs/0123456789abcdef0000", True),
            ("/src/blobs/01234", False),  # Too short
            ("0123456789abcdef0000", True),
        ]
        for input, expected in TEST_CASES:
            self.assertEqual(
                build_utils.is_likely_content_hash_path(input),
                expected,
                msg=f"For input [{input}]",
            )


class TestBazelLauncher(BazelLauncher):
    """A BazelLauncher sub-class used to mock subprocess invocation.

    The class manages a FIFO of BazelCommandResult values that is
    filled by calling push_result(), and which is consumed when
    run_command() is called.
    """

    def __init__(self) -> None:
        """Create instance."""

        def log(msg: str) -> None:
            self.logs.append(msg)

        def log_error(msg: str) -> None:
            self.errors.append(msg)

        super().__init__("/path/to/bazel", log=log, log_err=log_error)
        self.commands: list[list[str]] = []
        self.result_queue: list[BazelCommandResult] = []
        self.logs: list[str] = []
        self.errors: list[str] = []

    def push_result(
        self, returncode: int = 0, stdout: str = "", stderr: str = ""
    ) -> None:
        """Add one result value to the FIFO.

        Args:
            returncode: Optional process return code. default to 0.
            stdout: Optional process stdout, as a string, default to empty.
            stderr: Optional process stderr, as a string, default to empty.
        """
        self.result_queue.append(BazelCommandResult(returncode, stdout, stderr))

    def run_command(self, cmd_args: list[str]) -> BazelCommandResult:
        """Simulate command invocation by popping one value from the FIFO.

        Args:
            cmd_args: Command arguments, these are simply saved into
                self.commands for later inspection.
        Returns:
            The BazelCommandResult at the start of the FIFO.
        Raises:
            AssertionError if there are no results in the FIFO.
        """
        self.commands.append(cmd_args[:])
        assert self.result_queue
        result = self.result_queue[0]
        self.result_queue = self.result_queue[1:]
        return result


class BazelQueryCacheTest(unittest.TestCase):
    def setUp(self) -> None:
        self._td = tempfile.TemporaryDirectory()
        self._root = Path(self._td.name)

    def tearDown(self) -> None:
        self._td.cleanup()

    def test_cache(self) -> None:
        launcher = TestBazelLauncher()
        cache_dir = self._root / "cache"
        cache = BazelQueryCache(cache_dir)

        launcher.push_result(stdout="some\nresult\nlines")
        result = cache.get_query_output("query", ["deps(//src:foo)"], launcher)

        self.assertListEqual(result, ["some", "result", "lines"])
        self.assertTrue(cache_dir.is_dir())
        key, args = cache.compute_cache_key_and_args(
            "query", ["deps(//src:foo)"]
        )
        key_file = cache_dir / f"{key}.json"
        self.assertTrue(key_file.exists())
        with key_file.open() as f:
            value = json.load(f)
        self.assertDictEqual(
            value,
            {
                "key_args": ["query", "deps(//src:foo)"],
                "output_lines": ["some", "result", "lines"],
            },
        )

        # A second call with the same query and arguments should retrieve
        # the result from the cache, and not run any command. To detect this
        # push a new launcher result, which will not be popped as no
        # command will be run by the cache.
        launcher.push_result(stdout="some\nother\nresult\nlines")
        result2 = cache.get_query_output("query", ["deps(//src:foo)"], launcher)
        self.assertListEqual(result2, result)

        # By changing the query type, this will pop the last pushed
        # result value, while creating a new cache entry.
        result3 = cache.get_query_output(
            "cquery", ["deps(//src:foo)"], launcher
        )
        self.assertListEqual(result3, ["some", "other", "result", "lines"])

        key3, args3 = cache.compute_cache_key_and_args(
            "cquery", ["deps(//src:foo)"]
        )
        self.assertNotEqual(key3, key)
        key3_file = cache_dir / f"{key3}.json"
        self.assertTrue(key3_file.exists())
        with key3_file.open() as f:
            value3 = json.load(f)
        self.assertDictEqual(
            value3,
            {
                "key_args": ["cquery", "deps(//src:foo)"],
                "output_lines": ["some", "other", "result", "lines"],
            },
        )

    def test_cache_with_starlark_file(self) -> None:
        launcher = TestBazelLauncher()
        cache_dir = self._root / "cache"
        cache = BazelQueryCache(cache_dir)

        # Create starlark file. Exact content does not matter as real Bazel
        # queries are not run by this test.
        starlark_file = self._root / "query.starlark"
        starlark_file.write_text("1")

        query_cmd = "query"
        query_args = [
            "deps(//src:foo)",
            "--output=starlark",
            "--starlark:file",
            str(starlark_file),
        ]

        # First invocation creates cache entry.
        launcher.push_result(stdout="some\nresult\nlines")
        result = cache.get_query_output(query_cmd, query_args, launcher)
        self.assertListEqual(result, ["some", "result", "lines"])
        self.assertTrue(cache_dir.is_dir())

        key, args = cache.compute_cache_key_and_args(query_cmd, query_args)
        key_file = cache_dir / f"{key}.json"
        self.assertTrue(key_file.exists())
        with key_file.open() as f:
            value = json.load(f)
        self.assertDictEqual(
            value,
            {
                "key_args": [query_cmd] + query_args,
                "output_lines": ["some", "result", "lines"],
            },
        )

        # Second invocation returns the cache value directly without
        # invoking anything.
        launcher.push_result(stdout="other\nresult\nlines")
        result2 = cache.get_query_output(query_cmd, query_args, launcher)
        self.assertEqual(result2, result)

        key, args = cache.compute_cache_key_and_args(query_cmd, query_args)

        # Now modifying the contenf ot the input file should change
        # the key value and force a launcher invocation. The arguments
        # will be the same though.
        starlark_file.write_text("2")
        result3 = cache.get_query_output(query_cmd, query_args, launcher)
        self.assertEqual(result3, ["other", "result", "lines"])

        key2, args2 = cache.compute_cache_key_and_args(query_cmd, query_args)
        self.assertNotEqual(key2, key)
        self.assertEqual(args, args2)

        # Now change the query args to use --starlark:file=FILE
        # This will end up creating a new cache key.
        query_args = [
            "deps(//src:foo)",
            "--output=starlark",
            f"--starlark:file={starlark_file}",
        ]
        key3, args3 = cache.compute_cache_key_and_args(query_cmd, query_args)
        self.assertNotEqual(key3, key)
        self.assertNotEqual(key3, key2)

        # Modifying the file should change the key, but not the args.
        starlark_file.write_text("3")
        key4, args4 = cache.compute_cache_key_and_args(query_cmd, query_args)
        self.assertNotEqual(key4, key3)
        self.assertNotEqual(key4, key2)
        self.assertNotEqual(key4, key)
        self.assertEqual(args3, args4)


if __name__ == "__main__":
    unittest.main()
