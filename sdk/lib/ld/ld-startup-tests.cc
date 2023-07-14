// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dlfcn.h>
#include <lib/elfldltl/container.h>
#include <lib/elfldltl/diagnostics.h>
#include <lib/elfldltl/dynamic.h>
#include <lib/elfldltl/link.h>
#include <lib/elfldltl/load.h>
#include <lib/elfldltl/testing/diagnostics.h>
#include <lib/elfldltl/testing/get-test-data.h>
#include <lib/elfldltl/vmar-loader.h>
#include <lib/elfldltl/vmo.h>
#include <lib/zx/channel.h>
#include <lib/zx/vmo.h>
#include <zircon/syscalls.h>

#include <string>
#include <string_view>
#include <vector>

#include <gtest/gtest.h>

namespace {

constexpr std::string_view kLdsoTestName = LD_STARTUP_TEST_LIB;

class LdsoTest : public testing::Test {
 public:
  void SetUp() override {
    Load();
    ASSERT_EQ(zx::channel::create(0, &local_channel_, &ldso_channel_), ZX_OK);
  }

  int InvokeLdso(int one, int two) {
    std::array<int, 2> array{one, two};
    EXPECT_EQ(local_channel_.write(0, array.data(), sizeof(array), nullptr, 0), ZX_OK);
    return entry_(ldso_channel_.get(), GetVdso());
  }

 private:
  using Elf = elfldltl::Elf<>;
  using ElfLoadInfo = elfldltl::LoadInfo<Elf, elfldltl::StdContainer<std::vector>::Container>;
  using Phdr = Elf::Phdr;

  void Load() {
    auto diag = elfldltl::testing::ExpectOkDiagnostics();

    zx::vmo vmo = elfldltl::testing::GetTestLibVmo(kLdsoTestName);
    if (HasFatalFailure()) {
      return;
    }

    elfldltl::UnownedVmoFile file{vmo.borrow(), diag};

    auto headers =
        elfldltl::LoadHeadersFromFile<Elf>(diag, file, elfldltl::NewArrayFromFile<Phdr>());
    ASSERT_TRUE(headers);
    auto& [ehdr, phdrs_result] = *headers;

    ASSERT_TRUE(phdrs_result);
    cpp20::span<const Phdr> phdrs = phdrs_result.get();

    ElfLoadInfo load_info;
    ASSERT_TRUE(elfldltl::DecodePhdrs(diag, phdrs, load_info.GetPhdrObserver(loader_.page_size())));

    ASSERT_TRUE(loader_.Load(diag, load_info, vmo.borrow()));

    entry_ = loader_.memory().GetPointer<std::remove_pointer_t<decltype(entry_)>>(ehdr.entry);
  }

  static void* GetVdso() {
    static void* vdso = [] {
      Dl_info info;
      EXPECT_TRUE(dladdr(reinterpret_cast<void*>(&_zx_process_exit), &info));
      EXPECT_STREQ(info.dli_fname, "<vDSO>");
      return info.dli_fbase;
    }();
    return vdso;
  }

  elfldltl::LocalVmarLoader loader_;
  int (*entry_)(zx_handle_t, void*) = nullptr;
  zx::channel local_channel_;
  zx::channel ldso_channel_;
};

TEST_F(LdsoTest, Basic) { EXPECT_EQ(this->InvokeLdso(21, 8), 29); }

}  // namespace
