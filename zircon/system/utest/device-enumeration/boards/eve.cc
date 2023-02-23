// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, EveTest) {
  static const char* kDevicePaths[] = {
      "sys/platform/pci/00:1f.3/intel-hda-000/output-stream-001",     // Controller
                                                                      // headphones/speakers.
      "sys/platform/pci/00:1f.3/intel-hda-000/output-stream-003",     // Controller
                                                                      // headphones/speakers.
      "sys/platform/pci/00:1f.3/intel-hda-000/input-stream-002",      // Controller mics.
      "sys/platform/pci/00:19.2/i2c-bus-9d64/i2c/i2c-0-57/max98927",  // Codec left speaker.
      "sys/platform/pci/00:19.2/i2c-bus-9d64/i2c/i2c-0-58/max98927",  // Codec right speaker.
      "sys/platform/pci/00:19.2/i2c-bus-9d64/i2c/i2c-0-19/alc5663",   // Codec headphones.
      "sys/platform/pci/00:19.2/i2c-bus-9d64/i2c/i2c-0-87/alc5514",   // Codec mics.
  };

  ASSERT_NO_FATAL_FAILURE(TestRunner(kDevicePaths, std::size(kDevicePaths)));
}

}  // namespace
