#!/usr/bin/env fuchsia-vendored-python

# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.


def f() -> None:
    print("lib.f")


def truthy() -> bool:
    return True


def falsy() -> bool:
    return False
