// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/driver-framework-migration-utils/dispatcher/testing/dfv2-driver-with-dispatcher.h"

#include <lib/async/cpp/irq.h>
#include <lib/async/cpp/task.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/zx/interrupt.h>
#include <zircon/errors.h>
#include <zircon/status.h>

namespace display::testing {

Dfv2DriverWithDispatcher::Dfv2DriverWithDispatcher(
    fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : fdf::DriverBase("dfv2-driver-with-dispatcher", std::move(start_args),
                      std::move(driver_dispatcher)) {}

Dfv2DriverWithDispatcher::~Dfv2DriverWithDispatcher() = default;

zx::result<> Dfv2DriverWithDispatcher::Start() {
  auto create_dispatcher_result =
      dispatcher_factory_.Create(kTestDriverDispatcherName, kTestDriverSchedulerRole);
  if (create_dispatcher_result.is_error()) {
    return create_dispatcher_result.take_error();
  }
  dispatcher_ = std::move(create_dispatcher_result).value();
  return zx::ok();
}

zx::result<> Dfv2DriverWithDispatcher::PostTask(fit::closure task) {
  return zx::make_result(async::PostTask(dispatcher_->async_dispatcher(), std::move(task)));
}

zx::result<> Dfv2DriverWithDispatcher::StartIrqHandler(zx::interrupt irq,
                                                       async::Irq::Handler handler) {
  zx_handle_t irq_handle = irq.get();
  IrqAndHandler irq_and_handler = {
      .irq = std::move(irq),
      .handler = std::make_unique<async::Irq>(irq_handle, ZX_SIGNAL_NONE, std::move(handler)),
  };
  zx::result<> result =
      zx::make_result(irq_and_handler.handler->Begin(dispatcher_->async_dispatcher()));
  if (result.is_error()) {
    return result.take_error();
  }
  irq_and_handlers_.push_back(std::move(irq_and_handler));
  return zx::ok();
}

}  // namespace display::testing

FUCHSIA_DRIVER_EXPORT(::display::testing::Dfv2DriverWithDispatcher);
