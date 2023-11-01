// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_TEST_LD_LOAD_ZIRCON_LDSVC_TESTS_BASE_H_
#define LIB_LD_TEST_LD_LOAD_ZIRCON_LDSVC_TESTS_BASE_H_

#include <lib/ld/testing/mock-loader-service.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>

#include <initializer_list>
#include <string_view>

#include "ld-load-tests-base.h"

namespace ld::testing {

// This is the common base class for test fixtures that use a
// fuchsia.ldsvc.Loader service.
//
// It takes calls giving ordered expectations for Loader service requests from
// the process under test.  These must be used after Load() and before Run()
// in test cases.
class LdLoadZirconLdsvcTestsBase : public LdLoadTestsBase {
 public:
  ~LdLoadZirconLdsvcTestsBase();

  // Expect the dynamic linker to send a Config(config) message.
  void LdsvcExpectConfig(std::string_view config);

  // Expect the dynamic linker to send a LoadObject(name) request, and return
  // the given VMO (or error).
  void LdsvcExpectLoadObject(std::string_view name, zx::result<zx::vmo> result);

  // This is shorthand for LdsvcExpectLoadObject with the VMO acquired from
  // elfldltl::testing::GetTestLibVmo.
  void LdsvcExpectLoadObject(std::string_view name);

  // This just is a shorthand for multiple LdsvcExpectLoadObject calls.
  void Needed(std::initializer_list<std::string_view> names) {
    for (std::string_view name : names) {
      LdsvcExpectLoadObject(name);
    }
  }

  // This just is a shorthand for multiple LdsvcExpectLoadObject calls.
  void Needed(std::initializer_list<std::pair<std::string_view, bool>> name_found_pairs) {
    for (auto [name, found] : name_found_pairs) {
      if (found) {
        LdsvcExpectLoadObject(name);
      } else {
        LdsvcExpectLoadObject(name, zx::error{ZX_ERR_NOT_FOUND});
      }
    }
  }

 protected:
  zx::channel GetLdsvc() {
    zx::channel ldsvc;
    if (mock_.Ready()) {
      ldsvc = mock_.client().TakeChannel();
    }
    return ldsvc;
  }

 private:
  void ReadyMock();

  MockLoaderService mock_;
};

}  // namespace ld::testing

#endif  // LIB_LD_TEST_LD_LOAD_ZIRCON_LDSVC_TESTS_BASE_H_
