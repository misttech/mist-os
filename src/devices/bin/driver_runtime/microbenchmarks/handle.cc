// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_runtime/handle.h"

#include <lib/fdf/cpp/channel.h>

#include <perftest/perftest.h>

#include "src/devices/bin/driver_runtime/channel.h"

namespace {

// These tests measure the times taken to create and close various types of
// fdf handles. Strictly speaking, they test creating fdf objects as
// well as creating handles.
//
// In each test, closing the handles is done implicitly by destructors.

bool ChannelCreateTest(perftest::RepeatState* state) {
  state->DeclareStep("create");
  state->DeclareStep("close");
  while (state->KeepRunning()) {
    auto channels = fdf::ChannelPair::Create(0);
    ZX_ASSERT(channels.status_value() == ZX_OK);
    state->NextStep();
  }
  return true;
}

bool ChannelGetObjectTest(perftest::RepeatState* state) {
  auto channels = fdf::ChannelPair::Create(0);
  ZX_ASSERT(channels.status_value() == ZX_OK);

  state->DeclareStep("MapValueToHandle");
  state->DeclareStep("GetObject");

  while (state->KeepRunning()) {
    fbl::RefPtr<driver_runtime::Channel> channel;
    driver_runtime::Handle* handle = driver_runtime::Handle::MapValueToHandle(channels->end0.get());
    ZX_ASSERT(handle);
    state->NextStep();
    zx_status_t status = handle->GetObject<driver_runtime::Channel>(&channel);
    ZX_ASSERT(status == ZX_OK);
  }

  return true;
}

void RegisterTests() {
  perftest::RegisterTest("HandleCreate_Channel", ChannelCreateTest);
  perftest::RegisterTest("HandleGetObject_Channel", ChannelGetObjectTest);
}
PERFTEST_CTOR(RegisterTests)

}  // namespace
