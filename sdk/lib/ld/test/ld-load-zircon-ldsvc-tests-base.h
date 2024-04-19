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

  // Expect the dynamic linker to send a LoadObject(name) request, and return
  // the given VMO (or error).
  void LdsvcExpectLoadObject(std::string_view name, zx::result<zx::vmo> result) {
    mock_.ExpectLoadObject(name, std::move(result));
  }

  // Prime the MockLoaderService with the VMO for a dependency by name,
  // and expect the MockLoader to load that dependency for the test.
  void LdsvcExpectDependency(std::string_view name) { mock_.ExpectDependency(name); }

  // This just is a shorthand for multiple LdsvcExpectDependency  calls.
  void Needed(std::initializer_list<std::string_view> names) { mock_.Needed(names); }

  // This just is a shorthand for multiple LdsvcExpectDependency  calls.
  void Needed(std::initializer_list<std::pair<std::string_view, bool>> name_found_pairs) {
    mock_.Needed(name_found_pairs);
  }

  zx::channel GetLdsvc() { return mock_.GetLdsvc(); }

 private:
  MockLoaderServiceForTest mock_;
};

}  // namespace ld::testing

#endif  // LIB_LD_TEST_LD_LOAD_ZIRCON_LDSVC_TESTS_BASE_H_
