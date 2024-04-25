// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "dl-impl-tests.h"

#include <fcntl.h>
#include <lib/elfldltl/testing/get-test-data.h>
#ifdef __Fuchsia__
#include <lib/ld/testing/test-vmo.h>
#include <zircon/dlfcn.h>
#endif

#include <filesystem>

#include <gtest/gtest.h>

namespace dl::testing {

namespace {

// These startup modules should not be retrieved from the filesystem.
void FileCheck(std::string_view filename) {
  EXPECT_NE(filename, ld::abi::Abi<>::kSoname.str());
#ifdef __Fuchsia__
  EXPECT_NE(filename, ld::testing::GetVdsoSoname().str());
#endif
}

}  // namespace

#ifdef __Fuchsia__

std::optional<TestFuchsia::File> TestFuchsia::RetrieveFile(Diagnostics& diag,
                                                           std::string_view filename) {
  FileCheck(filename);

  // TODO(caslyn): We should avoid altering the global loader service state in
  // this test class. Instead RetrieveFile will become an object method
  // on DlImplLoadTestsBase which will have direct access to the mock loader,
  // and we will remove the TestOS class abstraction (Loader/File aliases
  // will be moved to the DlImplLoadTestsBase and passed separately to `Open`).

  // Obtain and save a handle to the loader installed for the test to make
  // fuchsia.ldsvc/Loader.LoadObject requests to.
  constexpr auto init_ldsvc = []() {
    zx::unowned_channel channel{dl_set_loader_service(ZX_HANDLE_INVALID)};
    EXPECT_TRUE(channel->is_valid());
    zx_handle_t reset = dl_set_loader_service(channel->get());
    EXPECT_EQ(reset, ZX_HANDLE_INVALID);
    return fidl::UnownedClientEnd<fuchsia_ldsvc::Loader>{channel};
  };
  static const auto ldsvc_endpoint = init_ldsvc();

  fidl::Arena arena;
  if (auto result = fidl::WireCall(ldsvc_endpoint)->LoadObject({arena, filename}); result.ok()) {
    if (auto load_result = result.Unwrap(); load_result->rv == ZX_OK) {
      return File{std::move(load_result->object), diag};
    }
  }

  diag.SystemError("cannot open ", filename);
  return std::nullopt;
}

#endif

std::optional<TestPosix::File> TestPosix::RetrieveFile(Diagnostics& diag,
                                                       std::string_view filename) {
  FileCheck(filename);
  std::filesystem::path path = elfldltl::testing::GetTestDataPath(filename);
  if (fbl::unique_fd fd{open(path.c_str(), O_RDONLY)}) {
    return File{std::move(fd), diag};
  }
  diag.SystemError("cannot open ", filename);
  return std::nullopt;
}

}  // namespace dl::testing
