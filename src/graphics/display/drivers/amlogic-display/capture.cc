// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/capture.h"

#include <lib/ddk/debug.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/result.h>
#include <threads.h>
#include <zircon/status.h>
#include <zircon/threads.h>

#include <memory>
#include <utility>

#include <fbl/alloc_checker.h>

#include "src/graphics/display/drivers/amlogic-display/board-resources.h"

namespace amlogic_display {

// static
zx::result<std::unique_ptr<Capture>> Capture::Create(ddk::PDevFidl& platform_device,
                                                     OnCaptureCompleteHandler on_capture_complete) {
  zx::result<zx::interrupt> capture_interrupt_result =
      GetInterrupt(InterruptResourceIndex::kVid1Write, platform_device);
  if (capture_interrupt_result.is_error()) {
    return capture_interrupt_result.take_error();
  }

  fbl::AllocChecker alloc_checker;
  auto capture = fbl::make_unique_checked<Capture>(
      &alloc_checker, std::move(capture_interrupt_result).value(), std::move(on_capture_complete));
  if (!alloc_checker.check()) {
    zxlogf(ERROR, "Out of memory while allocating Capture");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  zx::result<> init_result = capture->Init();
  if (init_result.is_error()) {
    zxlogf(ERROR, "Failed to initalize Capture: %s", init_result.status_string());
    return init_result.take_error();
  }

  return zx::ok(std::move(capture));
}

Capture::Capture(zx::interrupt capture_finished_interrupt,
                 OnCaptureCompleteHandler on_capture_complete)
    : capture_finished_irq_(std::move(capture_finished_interrupt)),
      on_capture_complete_(std::move(on_capture_complete)) {}

Capture::~Capture() {
  // In order to shut down the interrupt handler and join the thread, the
  // interrupt must be destroyed first.
  if (capture_finished_irq_.is_valid()) {
    zx_status_t status = capture_finished_irq_.destroy();
    if (status != ZX_OK) {
      zxlogf(ERROR, "Capture done interrupt destroy failed: %s", zx_status_get_string(status));
    }
  }

  if (interrupt_thread_.has_value()) {
    zx_status_t status = thrd_status_to_zx_status(thrd_join(*interrupt_thread_, nullptr));
    if (status != ZX_OK) {
      zxlogf(ERROR, "Capture done interrupt thread join failed: %s", zx_status_get_string(status));
    }
  }
}

zx::result<> Capture::Init() {
  thrd_t interrupt_thread;
  zx_status_t status = thrd_status_to_zx_status(thrd_create_with_name(
      &interrupt_thread,
      [](void* arg) { return reinterpret_cast<Capture*>(arg)->InterruptThreadEntryPoint(); },
      /*arg=*/this,
      /*name=*/"capture-interrupt-thread"));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to create interrupt thread: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  interrupt_thread_.emplace(interrupt_thread);
  return zx::ok();
}

int Capture::InterruptThreadEntryPoint() {
  while (true) {
    zx::time timestamp;
    zx_status_t status = capture_finished_irq_.wait(&timestamp);
    if (status == ZX_ERR_CANCELED) {
      zxlogf(INFO, "Capture finished interrupt wait is cancelled. Stopping interrupt thread.");
      break;
    }
    if (status != ZX_OK) {
      zxlogf(ERROR, "Capture finished interrupt wait failed: %s", zx_status_get_string(status));
      break;
    }

    OnCaptureComplete();
  }

  return 0;
}

void Capture::OnCaptureComplete() { on_capture_complete_(); }

}  // namespace amlogic_display
