// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_TESTING_LOADER_H_
#define SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_TESTING_LOADER_H_

#include <lib/elfldltl/container.h>
#include <lib/elfldltl/fd.h>
#include <lib/elfldltl/load.h>
#include <lib/elfldltl/mmap-loader.h>

#include <optional>
#include <string_view>
#include <vector>

#include <fbl/unique_fd.h>
#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "lib/elfldltl/testing/diagnostics.h"

#ifdef __Fuchsia__
#include <lib/elfldltl/vmar-loader.h>
#include <lib/elfldltl/vmo.h>
#include <lib/zx/vmar.h>
#endif

#include "get-test-data.h"

namespace elfldltl::testing {

// Each struct *LoaderTraits defines a few hooks so different kinds of loading
// can be tested with common code.

#ifdef __Fuchsia__
struct LocalVmarLoaderTraits {
  // The Loader object is move-constructible and move-assignable.
  // It might be default-constructible for not.
  using Loader = elfldltl::LocalVmarLoader;

  // This returns a Loader usable for in-process testing.
  static Loader MakeLoader(const zx::vmar& vmar = *zx::vmar::root_self()) {
    return elfldltl::LocalVmarLoader{vmar};
  }

  // This is a T(std::string_view) function that takes a string name of a test
  // module and returns some move-only type T representing the file's contents.
  // It will return a default-constructed T after having failed gtest
  // assertions if there's an error, so check HasFatalFailure().
  static constexpr auto TestLibProvider = GetTestLibVmo;

  // This returns some object that provides the File API, given the kind of
  // object that TestLibProvider returns.  The diagnostics object is expected
  // to be something that causes gtest failure if its error-reporting methods
  // are called.
  template <class Diagnostics>
  static auto MakeFile(zx::unowned_vmo vmo, Diagnostics& diagnostics) {
    return elfldltl::UnownedVmoFile(vmo->borrow(), diagnostics);
  }

  // This takes the return value of TestLibProvider and returns the argument to
  // pass to Loader::Load.
  static zx::unowned_vmo LoadFileArgument(const zx::vmo& vmo) { return vmo.borrow(); }

  // This indicates that the Loader::memory() method is available.
  static constexpr bool kHasMemory = true;
};

struct RemoteVmarLoaderTraits : public LocalVmarLoaderTraits {
  using Loader = elfldltl::RemoteVmarLoader;

  static Loader MakeLoader(const zx::vmar& vmar = *zx::vmar::root_self()) {
    return elfldltl::RemoteVmarLoader{vmar};
  }

  // No Loader::memory() method is available.
  static constexpr bool kHasMemory = false;
};
#endif

struct MmapLoaderTraits {
  using Loader = elfldltl::MmapLoader;

  static auto MakeLoader() { return elfldltl::MmapLoader{}; }

  static constexpr auto TestLibProvider = GetTestLib;

  template <class Diagnostics>
  static auto MakeFile(int fd, Diagnostics& diagnostics) {
    return elfldltl::FdFile(fd, diagnostics);
  }

  static int LoadFileArgument(const fbl::unique_fd& fd) { return fd.get(); }

  // This indicates that the Loader::memory() method is available.
  static constexpr bool kHasMemory = true;
};

using LoaderTypes = ::testing::Types<
#ifdef __Fuchsia__
    LocalVmarLoaderTraits, RemoteVmarLoaderTraits,
#endif
    MmapLoaderTraits>;

// This can be used as the base class for a test fixture, either via:
//   TYPED_TEST_SUITE(DerivedTests, LoaderTypes);
//   TYPED_TEST(DerivedTest, ...) {
// or just as a single class given a specific LoaderTraits parameter.
// The default template parameter uses POSIX-compatible mechanism that
// works on both Fuchsia and POSIX-like systems.
template <class Traits = MmapLoaderTraits, class Elf = Elf<>>
class LoadTests : public ::testing::Test {
 public:
  using Loader = typename Traits::Loader;
  using LoadInfo = LoadInfo<Elf, StdContainer<std::vector>::Container>;
  using Addr = typename Elf::Addr;
  using Phdr = typename Elf::Phdr;

  struct LoadResult {
    Loader loader;
    typename elfldltl::NewArrayFromFile<Phdr>::Result phdrs;
    Addr entry;
    LoadInfo info;
  };

  template <typename... LoaderArgs>
  void Load(std::string_view name, std::optional<LoadResult>& result, LoaderArgs&&... loader_args) {
    result = std::nullopt;

    auto lib = Traits::TestLibProvider(name);
    ASSERT_FALSE(this->HasFailure());

    auto diag = ExpectOkDiagnostics();
    auto file = Traits::MakeFile(Traits::LoadFileArgument(lib), diag);

    auto headers =
        elfldltl::LoadHeadersFromFile<Elf>(diag, file, elfldltl::NewArrayFromFile<Phdr>());
    ASSERT_TRUE(headers);
    auto& [ehdr, phdrs_result] = *headers;

    ASSERT_TRUE(phdrs_result);
    result = LoadResult{
        .loader = Traits::MakeLoader(std::forward<LoaderArgs>(loader_args)...),
        .phdrs = std::move(phdrs_result),
        .entry = ehdr.entry,
    };

    cpp20::span<const Phdr> phdrs = result->phdrs.get();
    ASSERT_TRUE(elfldltl::DecodePhdrs(diag, phdrs,
                                      result->info.GetPhdrObserver(result->loader.page_size())));

    ASSERT_TRUE(result->loader.Load(diag, result->info, Traits::LoadFileArgument(lib)));
  }
};

}  // namespace elfldltl::testing

#endif  // SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_TESTING_LOADER_H_
