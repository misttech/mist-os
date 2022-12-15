// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <lib/elfldltl/container.h>
#include <lib/elfldltl/dynamic.h>
#include <lib/elfldltl/fd.h>
#include <lib/elfldltl/link.h>
#include <lib/elfldltl/load.h>
#include <lib/elfldltl/mmap-loader.h>
#include <lib/fit/defer.h>

#include <vector>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "test-data.h"
#include "tests.h"

namespace {

struct LoadOptions {
  bool commit = true;
  bool reloc = true;
};

class ElfldltlLoaderTests : public testing::Test {
 public:
  void TearDown() override {
    if (!mapping_.empty()) {
      munmap(mapping_.data(), mapping_.size());
    }
  }

  void Load(std::string_view so_path, LoadOptions options = {}) {
    using Elf = elfldltl::Elf<>;
    using Phdr = Elf::Phdr;
    using Dyn = Elf::Dyn;

    auto diag = ExpectOkDiagnostics();

    fbl::unique_fd fd = GetTestLib(so_path);
    ASSERT_TRUE(fd) << so_path;

    elfldltl::FdFile fdfile{fd.get(), diag};

    auto headers =
        elfldltl::LoadHeadersFromFile<Elf>(diag, fdfile, elfldltl::NewArrayFromFile<Phdr>());
    ASSERT_TRUE(headers);
    auto& [ehdr, phdrs_result] = *headers;

    ASSERT_TRUE(phdrs_result);
    cpp20::span<const Phdr, cpp20::dynamic_extent> phdrs = phdrs_result.get();

    elfldltl::MmapLoader loader;
    elfldltl::LoadInfo<Elf, elfldltl::StdContainer<std::vector>::Container> load_info;
    ASSERT_TRUE(elfldltl::DecodePhdrs(diag, phdrs, load_info.GetPhdrObserver(loader.page_size())));

    ASSERT_TRUE(loader.Load(diag, load_info, fd.get()));

    elfldltl::DirectMemory& mem = loader.memory();

    if (options.reloc) {
      std::optional<Phdr> ph;
      elfldltl::DecodePhdrs(diag, phdrs, elfldltl::PhdrDynamicObserver<Elf>(ph));
      ASSERT_TRUE(ph);

      auto dyn = mem.ReadArray<Dyn>(ph->vaddr(), ph->filesz() / sizeof(Dyn));
      ASSERT_TRUE(dyn);

      elfldltl::RelocationInfo<Elf> relocInfo;
      ASSERT_TRUE(elfldltl::DecodeDynamic(diag, mem, *dyn,
                                          elfldltl::DynamicRelocationInfoObserver(relocInfo)));

      ASSERT_TRUE(
          RelocateRelative(mem, relocInfo, reinterpret_cast<uintptr_t>(mem.image().data())));
    }

    entry_ = mem.GetPointer<std::remove_pointer_t<decltype(entry_)>>(ehdr.entry);
    if (options.commit) {
      mapping_ = mem.image();
      std::move(loader).Commit();
    }
    EXPECT_EQ(mapping_.empty(), !options.commit);
  }

  template <typename T>
  T* entry() const {
    return reinterpret_cast<T*>(entry_);
  }

 private:
  void (*entry_)() = nullptr;
  cpp20::span<std::byte> mapping_;
};

TEST_F(ElfldltlLoaderTests, Basic) {
  Load(kRet24, {.reloc = false});

  EXPECT_EQ(entry<decltype(Return24)>()(), 24);
}

TEST_F(ElfldltlLoaderTests, UnmapCorrectly) {
  Load(kRet24, {.commit = false, .reloc = false});

  EXPECT_DEATH(entry<decltype(Return24)>()(), "");
}

TEST_F(ElfldltlLoaderTests, DataSegments) {
  Load(kNoXSegment);

  TestData* data = entry<TestData>();
  EXPECT_EQ(*data->rodata, 5);
  EXPECT_EQ(*data->data, 18);
  EXPECT_EQ(data->data[kSmallDataCount - 1], 1);
  EXPECT_EQ(*data->bss, 0);

  *data->data = 1;
  *data->bss = 2;
  int* rodata = const_cast<int*>(data->rodata);
  EXPECT_DEATH(*rodata = 3, "");
}

TEST_F(ElfldltlLoaderTests, LargeDataSegment) {
  Load(kNoXSegmentLargeData);

  TestData* data = entry<TestData>();
  EXPECT_EQ(*data->rodata, 5);
  EXPECT_EQ(*data->data, 18);
  EXPECT_EQ(data->data[kLargeDataCount - 1], 1);
  EXPECT_EQ(*data->bss, 0);

  *data->data = 1;
  data->data[kLargeDataCount - 1] = 9;
  *data->bss = 2;
  int* rodata = const_cast<int*>(data->rodata);
  EXPECT_DEATH(*rodata = 3, "");
}

}  // namespace
