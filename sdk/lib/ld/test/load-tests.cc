// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/elfldltl/testing/loader.h>
#include <lib/ld/abi.h>

#include <optional>
#include <string>
#include <string_view>

#include <gtest/gtest.h>

#ifdef __Fuchsia__
#include "ld-startup-in-process-tests-zircon.h"
#else
#include "ld-startup-in-process-tests-posix.h"
#endif

namespace {

template <class LoaderTraits>
class LdLoadTests : public elfldltl::testing::LoadTests<LoaderTraits> {
 public:
  using Base = elfldltl::testing::LoadTests<LoaderTraits>;
  using typename Base::Loader;
  using typename Base::LoadResult;

  template <class Launch>
  void LaunchExecutable(std::string_view executable_name, Launch&& launch) {
    std::optional<LoadResult> result;
    ASSERT_NO_FATAL_FAILURE(std::apply(
        [this, &result](auto&&... args) {
          this->Load(kLdStartupName, result,
                     // LoaderArgs() gives args for LoaderTraits::MakeLoader().
                     std::forward<decltype(args)>(args)...);
        },
        launch.LoaderArgs()));
    entry_ = result->entry + result->loader.load_bias();
    ASSERT_NO_FATAL_FAILURE(launch.AfterLoad(std::move(result->loader)));
    ASSERT_NO_FATAL_FAILURE(launch.SendExecutable(executable_name, *this));
  }

  template <class Launch>
  int Invoke(Launch&& launch) {
    return std::forward<Launch>(launch).Call(entry_);
  }

 private:
#ifdef __Fuchsia__
  static constexpr std::string_view kLibprefix = LD_STARTUP_TEST_LIBPREFIX;
  inline static const std::string kLdStartupName =
      std::string("test/lib/") + std::string(kLibprefix) + std::string(ld::abi::kInterp);
#else
  static constexpr std::string_view kLdStartupName = ld::abi::kInterp;
#endif

  uintptr_t entry_ = 0;
};

#ifdef __Fuchsia__
// Don't test MmapLoaderTraits on Fuchsia since it can't clean up after itself.
using LoaderTypes = ::testing::Types<elfldltl::testing::LocalVmarLoaderTraits,
                                     elfldltl::testing::RemoteVmarLoaderTraits>;
#else
using LoaderTypes = elfldltl::testing::LoaderTypes;
#endif

TYPED_TEST_SUITE(LdLoadTests, LoaderTypes);

TYPED_TEST(LdLoadTests, Basic) {
  constexpr int kReturnValue = 17;

  ld::testing::InProcessTestLaunch launch;
  ASSERT_NO_FATAL_FAILURE(launch.Init());

  ASSERT_NO_FATAL_FAILURE(this->LaunchExecutable("ret17", launch));

  EXPECT_EQ(this->Invoke(launch), kReturnValue);

  if constexpr (ld::testing::InProcessTestLaunch::kHasLog) {
    EXPECT_EQ(launch.CollectLog(), "");
  }
}

}  // namespace
