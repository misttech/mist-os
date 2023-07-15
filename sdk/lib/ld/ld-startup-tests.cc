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
#include <lib/elfldltl/testing/loader.h>

#ifdef __Fuchsia__
#include <lib/zx/channel.h>
#include <zircon/syscalls.h>
#endif

#include <string_view>
#include <type_traits>

#include <gtest/gtest.h>

namespace {

constexpr std::string_view kLdStartupName = LD_STARTUP_TEST_LIB;

// The in-process tests here work by doing ELF loading approximately as the
// system program loader would, but into this process that's running the test.
// Once the dynamic linker has been loaded, the InProcessTestLaunch object
// knows how its entry point wants to be called.  It's responsible for
// collecting the information to be passed to the dynamic linker, and then
// doing the call into its entry point to emulate what it would expect from the
// program loader starting an initial thread.
//
// The simple first version just takes a single argument string that the
// dynamic linker will receive.

#ifdef __Fuchsia__

// On Fuchsia this means packing a message on the bootstrap channel.  The entry
// point receives the bootstrap channel (zx_handle_t) and the base address of
// the vDSO.
class InProcessTestLaunch {
 public:
  // The object is default-constructed so Init() can be called inside
  // ASSERT_NO_FATAL_FAILURE(...).
  void Init(std::string_view str) {
    zx::channel write_bootstrap;
    ASSERT_EQ(zx::channel::create(0, &write_bootstrap, &read_bootstrap_), ZX_OK);
    ASSERT_EQ(write_bootstrap.write(0, str.data(), static_cast<uint32_t>(str.size()), nullptr, 0),
              ZX_OK);
  }

  int Call(uintptr_t entry) {
    auto fn = reinterpret_cast<EntryFunction*>(entry);
    return fn(read_bootstrap_.release(), GetVdso());
  }

 private:
  using EntryFunction = int(zx_handle_t, void*);

  static void* GetVdso() {
    static void* vdso = [] {
      Dl_info info;
      EXPECT_TRUE(dladdr(reinterpret_cast<void*>(&_zx_process_exit), &info));
      EXPECT_STREQ(info.dli_fname, "<vDSO>");
      return info.dli_fbase;
    }();
    return vdso;
  }

  // This is the receive end of the channel, transferred to the "new process".
  zx::channel read_bootstrap_;
};

#else  // ! __Fuchsia__

// On POSIX-like systems this eventually will mean a canonical stack setup.
// For now, we're just passing the string pointer as is.
class InProcessTestLaunch {
 public:
  void Init(std::string_view str) { str_ = str; }

  int Call(uintptr_t entry) {
    auto fn = reinterpret_cast<EntryFunction*>(entry);
    return fn(str_.c_str());
  }

 private:
  using EntryFunction = int(const char*);

  std::string str_;
};

#endif  // __Fuchsia__

template <class LoaderTraits>
class LdStartupTests : public elfldltl::testing::LoadTests<LoaderTraits> {
 public:
  using Base = elfldltl::testing::LoadTests<LoaderTraits>;
  using typename Base::Loader;
  using typename Base::LoadResult;

  void SetUp() override { Load(); }

  void Load() {
    std::optional<LoadResult> result;
    ASSERT_NO_FATAL_FAILURE(Base::Load(kLdStartupName, result));
    loader_ = std::move(result->loader);
    entry_ = result->entry + loader_->load_bias();
  }

  uintptr_t entry() const { return entry_; }

 private:
  std::optional<Loader> loader_;
  uintptr_t entry_ = 0;
};

TYPED_TEST_SUITE(LdStartupTests, elfldltl::testing::LoaderTypes);

TYPED_TEST(LdStartupTests, Basic) {
  // The skeletal dynamic linker is hard-coded now to read its argument string
  // and return its length.
  constexpr std::string_view kArgument = "Lorem ipsum dolor sit amet";
  constexpr int kReturnValue = static_cast<int>(kArgument.size());

  InProcessTestLaunch launch;
  ASSERT_NO_FATAL_FAILURE(launch.Init(kArgument));

  EXPECT_EQ(launch.Call(this->entry()), kReturnValue);
}

}  // namespace
