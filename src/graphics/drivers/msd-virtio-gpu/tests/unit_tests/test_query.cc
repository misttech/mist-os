// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "src/graphics/drivers/msd-virtio-gpu/src/msd_virtio_device.h"

class VirtioGpuControlTest : public VirtioGpuControl {
 public:
};

TEST(TestQuery, TBD) {
  VirtioGpuControlTest control;
  MsdVirtioDevice device(&control);
  // TODO(b/322043249): add query tests
}
