// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/scenic/tests/scenic_test.h"

#include <lib/sys/cpp/testing/component_context_provider.h>

#include "src/ui/scenic/lib/scheduling/constant_frame_predictor.h"

namespace scenic_impl::test {

void ScenicTest::SetUp() {
  sys::testing::ComponentContextProvider provider;
  context_ = provider.TakeContext();
  frame_scheduler_ = std::make_unique<scheduling::DefaultFrameScheduler>(
      std::make_unique<scheduling::ConstantFramePredictor>(/* static_vsync_offset */ zx::msec(5)));
  scenic_ = std::make_shared<Scenic>(context_.get());
  InitializeScenic(scenic_);
}

void ScenicTest::TearDown() {
  scenic_.reset();
  frame_scheduler_.reset();
}

void ScenicTest::InitializeScenic(std::shared_ptr<Scenic> scenic) {}

}  // namespace scenic_impl::test
