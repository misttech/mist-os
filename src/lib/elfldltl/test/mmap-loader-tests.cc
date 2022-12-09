// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <lib/elfldltl/container.h>
#include <lib/elfldltl/fd.h>
#include <lib/elfldltl/load.h>
#include <lib/elfldltl/mmap-loader.h>
#include <lib/fit/defer.h>

#include <vector>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "gtests.h"
#include "test-data.h"

namespace {

constexpr std::string_view kRet24 = "elfldltl-test-ret24.so";

class ElfldltlLoaderTests : public testing::Test {
 public:
  void TearDown() override {
    if (!mapping_.empty()) {
      munmap(mapping_.data(), mapping_.size());
    }
  }

  void Load(std::string_view so_path, bool commit = true) {
    using Elf = elfldltl::Elf<>;
    using Phdr = Elf::Phdr;

    auto diag = ExpectOkDiagnostics();

    std::filesystem::path path = GetTestDataPath(so_path);
    fbl::unique_fd fd{open(path.c_str(), O_RDONLY)};
    ASSERT_TRUE(fd) << path;

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

    entry_ = mem.GetPointer<std::remove_pointer_t<decltype(entry_)>>(ehdr.entry);
    if (commit) {
      mapping_ = mem.image();
      std::move(loader).Commit();
    }
    EXPECT_EQ(mapping_.empty(), !commit);
  }

  template <typename T>
  T* entry() const {
    static_assert(std::is_function_v<T>);
    return reinterpret_cast<T*>(entry_);
  }

 private:
  void (*entry_)() = nullptr;
  cpp20::span<std::byte> mapping_;
};

TEST_F(ElfldltlLoaderTests, Basic) {
  Load(kRet24);

  EXPECT_EQ(entry<decltype(Return24)>()(), 24);
}

TEST_F(ElfldltlLoaderTests, UnmapCorrectly) {
  Load(kRet24, false);

  EXPECT_DEATH(entry<decltype(Return24)>()(), "");
}

}  // namespace
