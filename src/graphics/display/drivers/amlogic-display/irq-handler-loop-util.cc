// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/irq-handler-loop-util.h"

#include <lib/async-loop/loop.h>

namespace amlogic_display {

async_loop_config_t CreateIrqHandlerAsyncLoopConfig() {
  async_loop_config_t config = kAsyncLoopConfigNeverAttachToThread;
  config.irq_support = true;
  return config;
}

}  // namespace amlogic_display
