// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_DDKTL_TESTS_METADATA_SERVER_TEST_FUCHSIA_HARDWARE_TEST_METADATA_H_
#define SRC_LIB_DDKTL_TESTS_METADATA_SERVER_TEST_FUCHSIA_HARDWARE_TEST_METADATA_H_

#include <fidl/fuchsia.hardware.test/cpp/fidl.h>

#include <ddktl/metadata_server.h>

namespace ddk {

template <>
struct ObjectDetails<fuchsia_hardware_test::Metadata> {
  inline static const char* Name = fuchsia_hardware_test::kMetadataTypeName;
};

template <>
struct ObjectDetails<fuchsia_hardware_test::wire::Metadata> {
  inline static const char* Name = fuchsia_hardware_test::kMetadataTypeName;
};

}  // namespace ddk

namespace ddk::test {

using MetadataServer = ddk::MetadataServer<fuchsia_hardware_test::wire::Metadata>;

}  // namespace ddk::test

#endif  // SRC_LIB_DDKTL_TESTS_METADATA_SERVER_TEST_FUCHSIA_HARDWARE_TEST_METADATA_H_
