// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>
#include <src/lib/fxl/command_line.h>
#ifndef VIRTMAGMA
#include "src/graphics/magma/tests/integration/conformance_config.h"  // nogncheck
#endif

#include "test_magma.h"

uint32_t gVendorId;

int main(int argc, char** argv) {
  testing::InitGoogleTest(&argc, argv);

  fxl::CommandLine command_line = fxl::CommandLineFromArgcArgv(argc, argv);
  std::string vendor_id_string;
#ifndef VIRTMAGMA
  auto c = conformance_config::Config::TakeFromStartupHandle();

  vendor_id_string = c.gpu_vendor_id();
#else
  command_line.GetOptionValue("vendor-id", &vendor_id_string);
#endif

  uint64_t vendor_id = strtoul(vendor_id_string.c_str(), nullptr, 0);
  gVendorId = static_cast<uint32_t>(vendor_id);
  if (gVendorId != vendor_id) {
    fprintf(stderr, "Invalid vendor_id: %s\n", vendor_id_string.c_str());
    return 1;
  }

  return RUN_ALL_TESTS();
}
