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
#include <lib/elfldltl/testing/loader.h>
#include <lib/elfldltl/vmar-loader.h>
#include <lib/elfldltl/vmo.h>
#include <lib/zx/channel.h>
#include <lib/zx/vmo.h>
#include <zircon/syscalls.h>

#include <string>
#include <string_view>
#include <type_traits>
#include <vector>

#include <gtest/gtest.h>

namespace {

constexpr std::string_view kLdStartupName = LD_STARTUP_TEST_LIB;

template <class LoaderTraits>
class LdStartupTests : public elfldltl::testing::LoadTests<LoaderTraits> {
 public:
  using Base = elfldltl::testing::LoadTests<LoaderTraits>;
  using typename Base::Loader;
  using typename Base::LoadResult;

  void SetUp() override {
    Load();
    ASSERT_EQ(zx::channel::create(0, &local_channel_, &remote_channel_), ZX_OK);
  }

  void Load() {
    std::optional<LoadResult> result;
    Base::Load(kLdStartupName, result);
    if (this->HasFatalFailure()) {
      return;
    }

    loader_ = std::move(result->loader);
    entry_ = result->entry + loader_->load_bias();
  }

  template <typename F>
  F* Entry() {
    static_assert(std::is_function_v<F>);
    return reinterpret_cast<F*>(entry_);
  }

  zx::channel& local_channel() { return local_channel_; }
  zx::channel& remote_channel() { return remote_channel_; }

  static void* GetVdso() {
    static void* vdso = [] {
      Dl_info info;
      EXPECT_TRUE(dladdr(reinterpret_cast<void*>(&_zx_process_exit), &info));
      EXPECT_STREQ(info.dli_fname, "<vDSO>");
      return info.dli_fbase;
    }();
    return vdso;
  }

 private:
  std::optional<Loader> loader_;
  uintptr_t entry_ = 0;
  zx::channel local_channel_;
  zx::channel remote_channel_;
};

TYPED_TEST_SUITE(LdStartupTests, elfldltl::testing::LoaderTypes);

TYPED_TEST(LdStartupTests, Basic) {
  std::array array{21, 8};
  EXPECT_EQ(this->local_channel().write(0, array.data(), sizeof(array), nullptr, 0), ZX_OK);

  auto entry = this->template Entry<int(zx_handle_t, void*)>();
  EXPECT_EQ(entry(this->remote_channel().get(), this->GetVdso()), 29);
}

}  // namespace
