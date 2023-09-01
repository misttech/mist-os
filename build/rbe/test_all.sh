#!/bin/bash -ev
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Manually run all python unit-tests in this dir.
# This is faster than `fx test //build/rbe`.
# Some tests require python protobufs to be compiled first,
# those are wrapped in a .sh script that adjusts
# PYTHONPATH accordingly.

rbe_dir="$(dirname $0)"

for t in "$rbe_dir"/*_test.py
do
  base="$(basename "$t" .py)"
  sh_test="$rbe_dir/$base.sh"
  if test -f "$sh_test"
  then
    echo "---- $sh_test ----"
    ./"$sh_test"
  else
    echo "---- $t ----"
    ./"$t"
  fi
done
