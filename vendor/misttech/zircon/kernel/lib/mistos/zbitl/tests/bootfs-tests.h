// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_ZBITL_TESTS_BOOTFS_TESTS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_ZBITL_TESTS_BOOTFS_TESTS_H_

#include <lib/zbitl/items/bootfs.h>
#include <lib/zbitl/view.h>

#include <iterator>

#include "array-tests.h"
#include "tests.h"
#include "vmo-tests.h"

template <typename TestTraits>
void CreateBootfs(typename TestTraits::Context& context,
                  zbitl::Bootfs<typename TestTraits::storage_type>& bootfs) {
  using Storage = typename TestTraits::storage_type;

  ktl::span<const char> buff;
  size_t size = 0;
  ASSERT_NO_FATAL_FAILURE(OpenTestDataZbi(TestDataZbiType::kBootfs, &buff, &size));

  // Read the ZBI containing the BOOTFS into memory.
  typename FblByteArrayTestTraits::Context zbi_context;
  ASSERT_NO_FATAL_FAILURE(FblByteArrayTestTraits::Create(std::move(buff), size, &zbi_context));
  zbitl::View view(zbi_context.TakeStorage());

  auto it = view.begin();
  ASSERT_EQ(std::next(it), view.end(), "expected a single BOOTFS item");
  ASSERT_EQ(uint32_t{ZBI_TYPE_STORAGE_BOOTFS}, it->header->type);

  // Ultimately we want to create an object of storage_type containing the
  // BOOTFS - and the preferred choice of test traits for creating storage
  // objects with prescribed contents is to use a fbl::unique_fd. Accordingly,
  // we decompress the BOOTFS into this form.
  const uint32_t bootfs_size = zbitl::UncompressedLength(*it->header);
  typename VmoTestTraits::Context decompressed_context;
  ASSERT_NO_FATAL_FAILURE(VmoTestTraits::Create(bootfs_size, &decompressed_context));

  zx::vmo bootfs_vmo = decompressed_context.TakeStorage();
  {
    auto result = view.CopyStorageItem(bootfs_vmo, it);
    ASSERT_FALSE(result.is_error(), "%s", ViewCopyErrorString(result.error_value()).c_str());
  }

  ASSERT_NO_FATAL_FAILURE(TestTraits::Create(std::move(bootfs_vmo), bootfs_size, &context));

  {
    auto result = zbitl::Bootfs<Storage>::Create(context.TakeStorage());
    ASSERT_FALSE(result.is_error(), "%s", BootfsErrorString(result.error_value()).c_str());
    bootfs = std::move(result.value());
  }

  {
    auto result = view.take_error();
    EXPECT_FALSE(result.is_error(), "%s", ViewErrorString(result.error_value()).c_str());
  }
}

template <typename BootfsView>
void TestFind(BootfsView& bootfs, std::initializer_list<std::string_view> path_parts,
              typename BootfsView::iterator expected_it) {
  auto match = bootfs.find(path_parts);
  auto result = bootfs.take_error();
  ASSERT_FALSE(result.is_error(), "%s", BootfsErrorString(result.error_value()).c_str());
  EXPECT_TRUE(expected_it == match);
}

template <typename TestTraits>
void TestBootfsIteration() {
  using namespace std::string_view_literals;
  using Storage = typename TestTraits::storage_type;

  typename TestTraits::Context context;
  zbitl::Bootfs<Storage> reader;
  ASSERT_NO_FATAL_FAILURE(CreateBootfs<TestTraits>(context, reader));

  auto bootfs = reader.root();
  uint32_t idx = 0;
  for (auto it = bootfs.begin(); it != bootfs.end(); ++it) {
    Bytes contents;
    ASSERT_NO_FATAL_FAILURE(TestTraits::Read(reader.storage(), it->data, it->size, &contents));

    ASSERT_NO_FATAL_FAILURE(TestFind(bootfs, {it->name}, it));
    ASSERT_NO_FATAL_FAILURE(TestFind(bootfs, {it->name}, it));
    switch (idx) {
      case 0:
        EXPECT_TRUE(it->name == "A.txt"sv);
        EXPECT_TRUE(contents ==
                    "Four score and seven years ago our fathers brought forth on this "
                    "continent, a new nation, conceived in Liberty, and dedicated to the "
                    "proposition that all men are created equal.");
        break;
      case 1:
        EXPECT_TRUE(it->name == "nested/B.txt"sv);
        ASSERT_NO_FATAL_FAILURE(TestFind(bootfs, {"nested"sv, "B.txt"sv}, it));
        EXPECT_TRUE(contents ==
                    "Now we are engaged in a great civil war, testing whether that nation, "
                    "or any nation so conceived and so dedicated, can long endure.");
        break;
      case 2:
        EXPECT_TRUE(it->name == "nested/again/C.txt"sv);
        ASSERT_NO_FATAL_FAILURE(TestFind(bootfs, {"nested/again"sv, "C.txt"sv}, it));
        ASSERT_NO_FATAL_FAILURE(TestFind(bootfs, {"nested"sv, "again/C.txt"sv}, it));
        ASSERT_NO_FATAL_FAILURE(TestFind(bootfs, {"nested"sv, "again"sv, "C.txt"sv}, it));
        EXPECT_TRUE(contents == "We are met on a great battle-field of that war.");
        break;
      default:
        __UNREACHABLE;
    }

    ++idx;
  }
  EXPECT_EQ(3u, idx, "we expect three files in the BOOTFS");

  {
    auto result = bootfs.take_error();
    EXPECT_FALSE(result.is_error(), "%s", BootfsErrorString(result.error_value()).c_str());
  }
}

template <typename TestTraits>
void TestBootfsSubdirectory() {
  using namespace std::string_view_literals;
  using Storage = typename TestTraits::storage_type;

  typename TestTraits::Context context;
  zbitl::Bootfs<Storage> reader;
  ASSERT_NO_FATAL_FAILURE(CreateBootfs<TestTraits>(context, reader));

  zbitl::BootfsView<Storage> root = reader.root();

  {
    auto result = root.subdir("nonexistent/directory"sv);
    ASSERT_TRUE(result.is_error());
    EXPECT_TRUE("unknown directory"sv == result.error_value().reason);
  }

  constexpr std::string_view kErrFileNotDir = "provided name is for a file, not a directory";
  {
    auto result = root.subdir("A.txt"sv);
    ASSERT_TRUE(result.is_error());
    EXPECT_TRUE(kErrFileNotDir == result.error_value().reason);
  }
  {
    auto result = root.subdir("nested/B.txt"sv);
    ASSERT_TRUE(result.is_error());
    EXPECT_TRUE(kErrFileNotDir == result.error_value().reason);
  }
  {
    auto result = root.subdir("nested/again/C.txt"sv);
    ASSERT_TRUE(result.is_error());
    EXPECT_TRUE(kErrFileNotDir == result.error_value().reason);
  }

  {
    auto result = root.subdir("nested"sv);
    ASSERT_FALSE(result.is_error());  // << BootfsErrorString(result.error_value());
    auto subdir = std::move(result).value();

    EXPECT_TRUE("nested"sv == subdir.directory());

    uint32_t idx = 0u;
    for (auto it = subdir.begin(); it != subdir.end(); ++it) {
      ASSERT_NO_FATAL_FAILURE(TestFind(subdir, {it->name}, it));
      switch (idx++) {
        case 0u:
          EXPECT_TRUE(it->name == "B.txt"sv);
          break;
        case 1u:
          EXPECT_TRUE(it->name == "again/C.txt"sv);
          ASSERT_NO_FATAL_FAILURE(TestFind(subdir, {"again"sv, "C.txt"sv}, it));
          break;
      }
    }
    EXPECT_EQ(2u, idx, "expected two files in the BOOTFS subdirectory");

    auto take_result = subdir.take_error();
    EXPECT_FALSE(take_result.is_error(), "%s",
                 BootfsErrorString(take_result.error_value()).c_str());
  }

  {
    auto result = root.subdir("nested/again"sv);
    ASSERT_FALSE(result.is_error(), "%s", BootfsErrorString(result.error_value()).c_str());
    auto subdir = std::move(result).value();

    EXPECT_TRUE("nested/again"sv == subdir.directory());

    uint32_t idx = 0u;
    for (auto it = subdir.begin(); it != subdir.end(); ++it) {
      ASSERT_NO_FATAL_FAILURE(TestFind(subdir, {it->name}, it));
      switch (idx++) {
        case 0u:
          EXPECT_TRUE(it->name == "C.txt"sv);
          break;
      }
    }
    EXPECT_EQ(1u, idx, "expected one files in the BOOTFS subdirectory");

    auto take_result = subdir.take_error();
    EXPECT_FALSE(take_result.is_error(), "%s",
                 BootfsErrorString(take_result.error_value()).c_str());
  }
}

#endif  // ZIRCON_KERNEL_LIB_MISTOS_ZBITL_TESTS_BOOTFS_TESTS_H_
