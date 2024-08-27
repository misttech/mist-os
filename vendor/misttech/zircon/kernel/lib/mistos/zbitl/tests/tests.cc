// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tests.h"

#include <lib/zbi-format/zbi.h>

#include <fbl/string.h>
#include <zxtest/zxtest.h>

namespace {

// ```
// `hexdump -v -e '1/1 "\\x%02x"' src/lib/zbitl/tests/data/empty.zbi`
// ```
alignas(ZBI_ALIGNMENT) constexpr char kEmptyZbi[] =
    "\x42\x4f\x4f\x54\x00\x00\x00\x00\xe6\xf7\x8c\x86\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00"
    "\x00\x29\x17\x78\xb5\xd6\xe8\x87\x4a";

// ```
// `hexdump -v -e '1/1 "\\x%02x"' src/lib/zbitl/tests/data/one-item.zbi`
// ```
alignas(ZBI_ALIGNMENT) constexpr char kOneItemZbi[] =
    "\x42\x4f\x4f\x54\x30\x00\x00\x00\xe6\xf7\x8c\x86\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00"
    "\x00\x29\x17\x78\xb5\xd6\xe8\x87\x4a\x49\x41\x52\x47\x0b\x00\x00\x00\x00\x00\x00\x00\x00\x00"
    "\x03\x00\x00\x00\x00\x00\x00\x00\x00\x00\x29\x17\x78\xb5\xa7\xe3\x0e\xd7\x68\x65\x6c\x6c\x6f"
    "\x20\x77\x6f\x72\x6c\x64\x00\x00\x00\x00\x00";

// ```
// hexdump -v -e '1/1 "\\x%02x"' src/lib/zbitl/tests/data/compressed-item.zbi
// ```
alignas(ZBI_ALIGNMENT) constexpr char kCompressedItemZbi[] =
    "\x42\x4f\x4f\x54\x48\x00\x00\x00\xe6\xf7\x8c\x86\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00"
    "\x00\x29\x17\x78\xb5\xd6\xe8\x87\x4a\x52\x44\x53\x4b\x23\x00\x00\x00\x1a\x00\x00\x00\x01\x00"
    "\x03\x00\x00\x00\x00\x00\x00\x00\x00\x00\x29\x17\x78\xb5\xc0\x91\xd8\xc8\x28\xb5\x2f\xfd\x20"
    "\x1a\xd1\x00\x00\x61\x62\x63\x64\x65\x66\x67\x68\x69\x6a\x6b\x6c\x6d\x6e\x6f\x70\x71\x72\x73"
    "\x74\x75\x76\x77\x78\x79\x7a\x00\x00\x00\x00\x00";

// ```
// hexdump -v -e '1/1 "\\x%02x"' src/lib/zbitl/tests/data/bad-crc-item.zbi
// ```
alignas(ZBI_ALIGNMENT) constexpr char kBadCrcItemZbi[] =
    "\x42\x4f\x4f\x54\x30\x00\x00\x00\xe6\xf7\x8c\x86\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00"
    "\x00\x29\x17\x78\xb5\xd6\xe8\x87\x4a\x49\x41\x52\x47\x0b\x00\x00\x00\x00\x00\x00\x00\x00\x00"
    "\x03\x00\x00\x00\x00\x00\x00\x00\x00\x00\x29\x17\x78\xb5\xa7\xe3\x0e\xd7\x68\x65\x6c\x6c\x6f"
    "\x20\x77\xaa\xaa\xaa\xaa\xaa\xaa\xaa\xaa\x00";

void GetExpectedPayloadCrc32(TestDataZbiType type, size_t idx, uint32_t* crc) {
  size_t num_items = GetExpectedNumberOfItems(type);
  ASSERT_LT(idx, num_items, "idx exceeds expected number of items in ZBI");

  switch (type) {
    case TestDataZbiType::kEmpty:
      // Assert would have already fired above.
      __UNREACHABLE;
    case TestDataZbiType::kOneItem:
      *crc = 3608077223;
      break;
    case TestDataZbiType::kCompressedItem:
      *crc = 4242380983;
      break;
    case TestDataZbiType::kBadCrcItem:
      // This function should not be called for this type.
      __UNREACHABLE;
    case TestDataZbiType::kMultipleSmallItems: {
      static const uint32_t crcs[] = {
          3172087020, 2653628068, 1659816855, 2798301622, 833025785,
          1420658445, 1308637244, 764240975,  2938513956, 3173475760,
      };
      *crc = crcs[idx];
      break;
    }
    case TestDataZbiType::kSecondItemOnPageBoundary: {
      static const uint32_t crcs[] = {2447293089, 3746526874};
      *crc = crcs[idx];
      break;
    }
    case TestDataZbiType::kBootfs:
      *crc = 3259698624;
      break;
  }
}

}  // namespace

size_t GetExpectedItemType(TestDataZbiType type) {
  switch (type) {
    case TestDataZbiType::kEmpty:
    case TestDataZbiType::kOneItem:
    case TestDataZbiType::kBadCrcItem:
    case TestDataZbiType::kMultipleSmallItems:
    case TestDataZbiType::kSecondItemOnPageBoundary:
      return ZBI_TYPE_IMAGE_ARGS;
    case TestDataZbiType::kCompressedItem:
      return ZBI_TYPE_STORAGE_RAMDISK;
    case TestDataZbiType::kBootfs:
      return ZBI_TYPE_STORAGE_BOOTFS;
  }
}

bool ExpectItemsAreCompressed(TestDataZbiType type) {
  switch (type) {
    case TestDataZbiType::kEmpty:
    case TestDataZbiType::kOneItem:
    case TestDataZbiType::kBadCrcItem:
    case TestDataZbiType::kMultipleSmallItems:
    case TestDataZbiType::kSecondItemOnPageBoundary:
      return false;
    case TestDataZbiType::kCompressedItem:
    case TestDataZbiType::kBootfs:
      return true;
  }
}

size_t GetExpectedNumberOfItems(TestDataZbiType type) {
  switch (type) {
    case TestDataZbiType::kEmpty:
      return 0;
    case TestDataZbiType::kOneItem:
    case TestDataZbiType::kCompressedItem:
    case TestDataZbiType::kBadCrcItem:
      return 1;
    case TestDataZbiType::kMultipleSmallItems:
      return 10;
    case TestDataZbiType::kSecondItemOnPageBoundary:
      return 2;
    case TestDataZbiType::kBootfs:
      return 1;
  }
}

void GetExpectedPayload(TestDataZbiType type, size_t idx, Bytes* contents) {
  size_t num_items = GetExpectedNumberOfItems(type);
  ASSERT_LT(idx, num_items, "idx exceeds expected number of items in ZBI");

  switch (type) {
    case TestDataZbiType::kEmpty:
      // Assert would have already fired above.
      __UNREACHABLE;
    case TestDataZbiType::kOneItem: {
      *contents = "hello world";
      return;
    }
    case TestDataZbiType::kCompressedItem: {
      *contents = "abcdefghijklmnopqrstuvwxyz";
      return;
    }
    case TestDataZbiType::kBadCrcItem: {
      *contents = "hello w\xaa\xaa\xaa\xaa";
      return;
    }
    case TestDataZbiType::kMultipleSmallItems: {
      static const char* const payloads[] = {
          "Four score and seven years ago our fathers brought forth on this continent, a new "
          "nation, conceived in Liberty, and dedicated to the proposition that all men are created "
          "equal.",
          "Now we are engaged in a great civil war, testing whether that nation, or any nation so "
          "conceived and so dedicated, can long endure.",
          "We are met on a great battle-field of that war.",
          "We have come to dedicate a portion of that field, as a final resting place for those "
          "who here gave their lives that that nation might live.",
          "It is altogether fitting and proper that we should do this.",
          "But, in a larger sense, we can not dedicate -- we can not consecrate -- we can not "
          "hallow -- this ground.",
          "The brave men, living and dead, who struggled here, have consecrated it, far above our "
          "poor power to add or detract.",
          "The world will little note, nor long remember what we say here, but it can never forget "
          "what they did here.",
          "It is for us the living, rather, to be dedicated here to the unfinished work which they "
          "who fought here have thus far so nobly advanced.",
          "It is rather for us to be here dedicated to the great task remaining before us -- that "
          "from these honored dead we take increased devotion to that cause for which they gave "
          "the last full measure of devotion -- that we here highly resolve that these dead shall "
          "not have died in vain -- that this nation, under God, shall have a new birth of freedom "
          "-- and that government of the people, by the people, for the people, shall not perish "
          "from the earth.",
      };
      *contents = payloads[idx];
      return;
    }
    case TestDataZbiType::kSecondItemOnPageBoundary: {
      static const char* const payloads[] = {
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
          "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX",
          "YYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYY",
      };
      *contents = payloads[idx];
      return;
    }
    case TestDataZbiType::kBootfs:
      // This function should not be called for this type.
      __UNREACHABLE;
  }
}

void GetExpectedPayloadWithHeader(TestDataZbiType type, size_t idx, Bytes* contents) {
  Bytes payload;
  ASSERT_NO_FATAL_FAILURE(GetExpectedPayload(type, idx, &payload));

  uint32_t crc;
  ASSERT_NO_FATAL_FAILURE(GetExpectedPayloadCrc32(type, idx, &crc));

  zbi_header_t header{};
  header.type = ZBI_TYPE_IMAGE_ARGS;
  header.magic = ZBI_ITEM_MAGIC;
  header.flags = ZBI_FLAGS_VERSION | ZBI_FLAGS_CRC32;
  header.length = static_cast<uint32_t>(payload.size());
  header.crc32 = crc;

  *contents = fbl::String{reinterpret_cast<char*>(&header), sizeof(zbi_header_t)};
  *contents += payload;
}

#include "data/bootfs.zbi.h"
#include "data/multiple-small-items.zbi.h"
#include "data/second-item-on-page-boundary.zbi.h"

void OpenTestDataZbi(TestDataZbiType type, ktl::span<const char>* buff, size_t* num_bytes) {
  switch (type) {
    case TestDataZbiType::kEmpty: {
      ktl::span<const char> zbi{kEmptyZbi, sizeof(kEmptyZbi) - 1};
      ASSERT_LE(zbi.size(), kMaxZbiSize, "file is too large");
      *num_bytes = zbi.size();
      *buff = std::move(zbi);
      return;
    }
    case TestDataZbiType::kOneItem: {
      ktl::span<const char> zbi{kOneItemZbi, sizeof(kOneItemZbi) - 1};
      ASSERT_LE(zbi.size(), kMaxZbiSize, "file is too large");
      *num_bytes = zbi.size();
      *buff = std::move(zbi);
      return;
    }
    case TestDataZbiType::kCompressedItem: {
      ktl::span<const char> zbi{kCompressedItemZbi, sizeof(kCompressedItemZbi) - 1};
      ASSERT_LE(zbi.size(), kMaxZbiSize, "file is too large");
      *num_bytes = zbi.size();
      *buff = std::move(zbi);
      return;
    }
    case TestDataZbiType::kBadCrcItem: {
      ktl::span<const char> zbi{kBadCrcItemZbi, sizeof(kBadCrcItemZbi) - 1};
      ASSERT_LE(zbi.size(), kMaxZbiSize, "file is too large");
      *num_bytes = zbi.size();
      *buff = std::move(zbi);
      return;
    }
    case TestDataZbiType::kMultipleSmallItems: {
      ktl::span<const char> zbi{kMultipleSmallItemsZbi, sizeof(kMultipleSmallItemsZbi) - 1};
      ASSERT_LE(zbi.size(), kMaxZbiSize, "file is too large");
      *num_bytes = zbi.size();
      *buff = std::move(zbi);
      return;
    }
    case TestDataZbiType::kSecondItemOnPageBoundary: {
      ktl::span<const char> zbi{kSecondItemOnPageBoundaryZbi,
                                sizeof(kSecondItemOnPageBoundaryZbi) - 1};
      ASSERT_LE(zbi.size(), kMaxZbiSize, "file is too large");
      *num_bytes = zbi.size();
      *buff = std::move(zbi);
      return;
    }
    case TestDataZbiType::kBootfs: {
      ktl::span<const char> zbi{kBootFsZbi, sizeof(kBootFsZbi) - 1};
      ASSERT_LE(zbi.size(), kMaxZbiSize, "file is too large");
      *num_bytes = zbi.size();
      *buff = std::move(zbi);
      return;
    }
  }
}
