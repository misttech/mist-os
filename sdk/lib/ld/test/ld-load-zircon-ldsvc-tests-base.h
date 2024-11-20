// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_TEST_LD_LOAD_ZIRCON_LDSVC_TESTS_BASE_H_
#define LIB_LD_TEST_LD_LOAD_ZIRCON_LDSVC_TESTS_BASE_H_

#include <lib/elfldltl/testing/get-test-data.h>
#include <lib/ld/testing/mock-loader-service.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>

#include <initializer_list>
#include <string_view>

#include "ld-load-tests-base.h"

namespace ld::testing {

// This is the common base class for test fixtures that use a
// fuchsia.ldsvc.Loader service and set expectations for the dependencies
// loaded by it. This class proxies calls to the MockLoaderServiceForTest and
// passes the function it should use to retrieve test VMO files.
//
// It takes calls giving ordered expectations for Loader service requests from
// the process under test.  These must be used after Load() and before Run()
// in test cases.
class LdLoadZirconLdsvcTestsBase : public LdLoadTestsBase {
 public:
  ~LdLoadZirconLdsvcTestsBase() = default;

  // Expect the dynamic linker to send a Config(config) message.
  void LdsvcExpectConfig(std::string_view config) { mock_.ExpectConfig(config); }

  // Prime the MockLoaderService with the VMO for a dependency by name,
  // and expect the MockLoader to load that dependency for the test.
  void LdsvcExpectDependency(std::string_view name) { mock_.ExpectDependency(name); }

  zx::channel TakeLdsvc() { return mock_.TakeLdsvc(); }

  zx::vmo GetLibVmo(std::string_view name) { return mock_.GetVmo(name); }

  static zx::vmo GetExecutableVmo(std::string_view executable) {
    const std::string executable_path =
        std::filesystem::path("test") / executable / "bin" / executable;
    return elfldltl::testing::GetTestLibVmo(executable_path);
  }

  void VerifyAndClearNeeded() { mock_.VerifyAndClearExpectations(); }

 protected:
  void LdsvcPathPrefix(std::string_view executable) {
    mock_.set_path_prefix(std::filesystem::path("test") / executable / "lib");
  }

  void LdsvcExpectNeeded() {
    for (const auto& [name, found] : TakeNeededLibs()) {
      if (found) {
        mock_.ExpectDependency(name);
      } else {
        mock_.ExpectMissing(name);
      }
    }
  }

  MockLoaderServiceForTest& mock() { return mock_; }

 private:
  MockLoaderServiceForTest mock_;
};

}  // namespace ld::testing

#endif  // LIB_LD_TEST_LD_LOAD_ZIRCON_LDSVC_TESTS_BASE_H_
