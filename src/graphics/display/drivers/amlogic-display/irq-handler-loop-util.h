// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_IRQ_HANDLER_LOOP_UTIL_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_IRQ_HANDLER_LOOP_UTIL_H_

#include <lib/async-loop/loop.h>

namespace amlogic_display {

// Creates a config for an async loop that is not registered to any thread and
// supports IRQ handling.
//
// Usage:
// ```
//     class Foo {
//      public:
//       Foo(): irq_handler_loop_config_(CreateIrqHandlerAsyncLoopConfig()),
//              irq_handler_loop_(&irq_handler_loop_config_) { ... }
//
//      private:
//       const async_loop_config_t irq_handler_loop_config_;
//       async::Loop irq_handler_loop_;
//       ...
//     };
// ```
async_loop_config_t CreateIrqHandlerAsyncLoopConfig();

}  // namespace amlogic_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_IRQ_HANDLER_LOOP_UTIL_H_
