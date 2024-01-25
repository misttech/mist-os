// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ld-load-zircon-ldsvc-tests-base.h"

#include <lib/elfldltl/testing/get-test-data.h>

namespace ld::testing {

constexpr std::string_view kLibprefix = LD_STARTUP_TEST_LIBPREFIX;

LdLoadZirconLdsvcTestsBase::~LdLoadZirconLdsvcTestsBase() = default;

void LdLoadZirconLdsvcTestsBase::LdsvcExpectConfig(std::string_view config) {
  ASSERT_NO_FATAL_FAILURE(ReadyMock());
  mock_.ExpectConfig(config, zx::ok());
}

void LdLoadZirconLdsvcTestsBase::LdsvcExpectLoadObject(std::string_view name,
                                                       zx::result<zx::vmo> result) {
  ASSERT_NO_FATAL_FAILURE(ReadyMock());
  mock_.ExpectLoadObject(name, std::move(result));
}

void LdLoadZirconLdsvcTestsBase::LdsvcExpectLoadObject(std::string_view name) {
  // TODO(https://fxbug.dev/42084623): We should add a LdsvcExpectConfig when the dynamic linker starts doing
  // so.
  const std::string path = std::filesystem::path("test") / "lib" / kLibprefix / name;
  zx::vmo vmo;
  ASSERT_NO_FATAL_FAILURE(vmo = elfldltl::testing::GetTestLibVmo(path));
  LdsvcExpectLoadObject(name, zx::ok(std::move(vmo)));
}

void LdLoadZirconLdsvcTestsBase::ReadyMock() {
  if (!mock_.Ready()) {
    ASSERT_NO_FATAL_FAILURE(mock_.Init());
  }
}

}  // namespace ld::testing
