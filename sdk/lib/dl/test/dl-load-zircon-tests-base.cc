// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include "dl-load-zircon-tests-base.h"

#include <lib/ld/testing/test-vmo.h>
#include <zircon/dlfcn.h>

namespace dl::testing {

void DlLoadZirconTestsBase::FileCheck(std::string_view filename) {
  Base::FileCheck(filename);
  EXPECT_NE(filename, ld::testing::GetVdsoSoname().str());
}

std::optional<DlLoadZirconTestsBase::File> DlLoadZirconTestsBase::RetrieveFile(
    Diagnostics& diag, std::string_view filename) {
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
      return DlLoadZirconTestsBase::File{std::move(load_result->object), diag};
    }
  }

  diag.SystemError("cannot open ", filename);
  return std::nullopt;
}

}  // namespace dl::testing
