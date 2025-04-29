# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import unittest

from json_get import Any, JsonGet, Maybe


class TestJsonGet(unittest.TestCase):
    def setUp(self) -> None:
        self.j = JsonGet(
            '{"a": 1, "b": {"bb": 2}, "s": [1, 2, {"x": 42}, {"x": 43}, {"y": 44}]}'
        )

    def test_call_on_miss(self) -> None:
        events: list[Any] = []
        m = self.j.match(
            {"c": Any},
            lambda _: events.append("bad"),
            no_match=lambda: events.append("good"),
        )
        self.assertEqual(["good"], events)
        self.assertIsNone(m)

    def test_call_on_hit(self) -> None:
        events: list[Any] = []
        m = self.j.match(
            {"a": Any},
            lambda m: events.append(m.a),
            no_match=lambda: events.append("bad"),
        )
        self.assertEqual([1], events)
        self.assertEqual(m.a, 1)

    def test_maybe(self) -> None:
        self.assertEqual(self.j.match({"a": Maybe}).a, 1)
        self.assertIsNone(self.j.match({"c": Maybe}).c)

    def test_none(self) -> None:
        self.assertEqual(self.j.match({"a": Any, "c": None}).a, 1)
        self.assertIsNone(self.j.match({"a": Any, "b": None}))

    def test_list_filter(self) -> None:
        self.assertEqual(self.j.match({"s": [2]}).s, [2])
        self.assertEqual(self.j.match({"s": [5]}).s, [])

    def test_nesting(self) -> None:
        self.assertEqual(self.j.match({"b": {"bb": Any}}).b.bb, 2)
        self.assertIsNone(self.j.match({"b": {"bb": 3}}))

    def test_initializations(self) -> None:
        input = "5"
        string_value = JsonGet(value=input)
        number_value = JsonGet(input)
        self.assertEqual(string_value.match(Any), "5")
        self.assertEqual(number_value.match(Any), 5)
