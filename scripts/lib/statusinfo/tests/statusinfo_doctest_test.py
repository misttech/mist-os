# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.


import doctest
import unittest

import statusinfo.statusinfo


def load_tests(
    _loader: unittest.TestLoader,
    tests: unittest.TestSuite,
    _ignore: unittest.TestLoader,
) -> unittest.TestSuite:
    tests.addTests(doctest.DocTestSuite(statusinfo.statusinfo))
    return tests
