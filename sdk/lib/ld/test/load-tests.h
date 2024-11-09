// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_TEST_LOAD_TESTS_H_
#define LIB_LD_TEST_LOAD_TESTS_H_

#include <lib/elfldltl/container.h>
#include <lib/elfldltl/memory.h>
#include <lib/elfldltl/testing/diagnostics.h>

#include <span>
#include <string>

#include <gtest/gtest.h>

#ifdef __Fuchsia__
#include "ld-remote-process-tests.h"
#include "ld-startup-create-process-tests.h"
#include "ld-startup-in-process-tests-zircon.h"
#include "ld-startup-spawn-process-tests-zircon.h"
#else
#include "ld-startup-in-process-tests-posix.h"
#include "ld-startup-spawn-process-tests-posix.h"
#endif

namespace ld::testing {

template <class Fixture>
using LdLoadTests = Fixture;

template <class Fixture>
using LdLoadFailureTests = Fixture;

// This lists all the types that are compatible with both LdLoadTests and LdLoadFailureTests.
template <class... Tests>
using TestTypes = ::testing::Types<
// TODO(https://fxbug.dev/42080760): The separate-process tests require symbolic
// relocation so they can make the syscall to exit. The spawn-process
// tests also need a loader service to get ld.so.1 itself.
#ifdef __Fuchsia__
    ld::testing::LdStartupCreateProcessTests<>,  //
    ld::testing::LdRemoteProcessTests,
#else
    ld::testing::LdStartupSpawnProcessTests,
#endif
    Tests...>;

// These types are meaningul for the successful tests, LdLoadTests.
using LoadTypes = TestTypes<ld::testing::LdStartupInProcessTests>;

// These types are the types which are compatible with the failure tests,
// LdLoadFailureTests.
using FailTypes = TestTypes<>;

TYPED_TEST_SUITE(LdLoadTests, LoadTypes);
TYPED_TEST_SUITE(LdLoadFailureTests, FailTypes);

template <template <class Diagnostics> class File, typename FileArg>
inline std::string FindInterp(FileArg&& file_arg) {
  std::string result;
  auto diag = elfldltl::testing::ExpectOkDiagnostics();
  File file{std::forward<FileArg>(file_arg), diag};
  auto scan_phdrs = [&diag, &file, &result](const auto& ehdr, const auto& phdrs) -> bool {
    for (const auto& phdr : phdrs) {
      if (phdr.type == elfldltl::ElfPhdrType::kInterp) {
        size_t len = phdr.filesz;
        if (len > 0) {
          auto read_chars = file.template ReadArrayFromFile<char>(
              phdr.offset,
              elfldltl::ContainerArrayFromFile<
                  elfldltl::StdContainer<std::vector>::Container<char>>(diag, "impossible"),
              len - 1);
          if (read_chars) {
            std::span<const char> chars = *read_chars;
            result = std::string_view{chars.data(), chars.size()};
            return true;
          }
        }
        return false;
      }
    }
    return true;
  };
  auto phdr_allocator = [&diag]<typename T>(size_t count) {
    return elfldltl::ContainerArrayFromFile<elfldltl::StdContainer<std::vector>::Container<T>>(
        diag, "impossible")(count);
  };
  EXPECT_TRUE(elfldltl::WithLoadHeadersFromFile(diag, file, phdr_allocator, scan_phdrs));
  return result;
}

}  // namespace ld::testing

#endif  // LIB_LD_TEST_LOAD_TESTS_H_
