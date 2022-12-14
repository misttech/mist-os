#!/usr/bin/env fuchsia-vendored-python

# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for gpt.py."""

import pathlib
import unittest

import gpt

_MY_DIR = pathlib.Path(__file__).parent

# A real disk image for testing against, generated via:
#  $ dd if=/dev/zero of=gpt_test_disk.img bs=4M count=1
#  $ sgdisk gpt_test_disk.img \
#      --clear \
#      --new=0:+0:+1MiB \
#      --change-name=1:foo \
#      --typecode=1:9B37FFF6-2E58-466A-983A-F7926D0B04E0 \
#      --partition-guid=1:A271DF36-BAAC-45B7-B806-A636996DAF75 \
#      --new=0:+0:+1MiB \
#      --change-name=2:bar \
#      --typecode=2:421A8BFC-85D9-4D85-ACDA-B64EEC0133E9 \
#      --partition-guid=2:8F1ED054-0475-43FE-AB8D-13C6E709D5A7
_TEST_DISK_PATH = _MY_DIR / "gpt_test_disk.img"

# Expected partitions based on the above disk image.
_TEST_DISK_FOO_PARTITION = gpt.Partition(
    index=1,
    name="foo",
    first_lba=2048,
    last_lba=4095,
    size=2048,
    type_guid="9B37FFF6-2E58-466A-983A-F7926D0B04E0",
    unique_guid="A271DF36-BAAC-45B7-B806-A636996DAF75",
    attr_flags="0000000000000000")
_TEST_DISK_BAR_PARTITION = gpt.Partition(
    index=2,
    name="bar",
    first_lba=4096,
    last_lba=6143,
    size=2048,
    type_guid="421A8BFC-85D9-4D85-ACDA-B64EEC0133E9",
    unique_guid="8F1ED054-0475-43FE-AB8D-13C6E709D5A7",
    attr_flags="0000000000000000")
_TEST_DISK_PARTITIONS = [_TEST_DISK_FOO_PARTITION, _TEST_DISK_BAR_PARTITION]


class GptTests(unittest.TestCase):

    def test_get_partitions(self):
        self.assertEqual(
            gpt.get_partitions(_TEST_DISK_PATH), _TEST_DISK_PARTITIONS)

    def test_get_partition_by_name(self):
        self.assertEqual(
            gpt.get_partition(_TEST_DISK_PATH, part_name="foo"),
            _TEST_DISK_FOO_PARTITION)

    def test_get_partition_by_index(self):
        self.assertEqual(
            gpt.get_partition(_TEST_DISK_PATH, index=2),
            _TEST_DISK_BAR_PARTITION)

    def test_get_partition_by_name_and_index(self):
        self.assertEqual(
            gpt.get_partition(_TEST_DISK_PATH, part_name="bar", index=2),
            _TEST_DISK_BAR_PARTITION)

    def test_get_partition_by_name_not_found(self):
        with self.assertRaises(ValueError):
            gpt.get_partition(_TEST_DISK_PATH, part_name="not_a_partition")

    def test_get_partition_by_index_not_found(self):
        with self.assertRaises(ValueError):
            gpt.get_partition(_TEST_DISK_PATH, index=5)

    def test_get_partition_by_name_and_index_bad_name(self):
        with self.assertRaises(ValueError):
            gpt.get_partition(_TEST_DISK_PATH, part_name="invalid", index=1)

    def test_get_partition_by_name_and_index_bad_index(self):
        with self.assertRaises(ValueError):
            gpt.get_partition(_TEST_DISK_PATH, part_name="bar", index=4)


if __name__ == "__main__":
    unittest.main()
