# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.


import doctest
import unittest

import statusinfo.statusinfo


def load_tests(_loader, tests: unittest.TestSuite, _ignore):
    tests.addTests(doctest.DocTestSuite(statusinfo.statusinfo))
    return tests
