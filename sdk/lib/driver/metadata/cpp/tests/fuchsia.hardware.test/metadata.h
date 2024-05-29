// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_METADATA_CPP_TESTS_FUCHSIA_HARDWARE_TEST_METADATA_H_
#define LIB_DRIVER_METADATA_CPP_TESTS_FUCHSIA_HARDWARE_TEST_METADATA_H_

#include <fidl/fuchsia.hardware.test/cpp/fidl.h>
#include <lib/driver/metadata/cpp/metadata_server.h>

#include "lib/driver/metadata/cpp/metadata.h"

namespace fdf_metadata {

template <>
struct ObjectDetails<fuchsia_hardware_test::Metadata> {
  inline static const char* Name = fuchsia_hardware_test::kService;
};

}  // namespace fdf_metadata

namespace fuchsia_hardware_test {

using MetadataServer = fdf_metadata::MetadataServer<Metadata>;

}  // namespace fuchsia_hardware_test

#endif  // LIB_DRIVER_METADATA_CPP_TESTS_FUCHSIA_HARDWARE_TEST_METADATA_H_
