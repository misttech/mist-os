// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "gpio-test.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/platform-defs.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include <memory>

#include <fbl/alloc_checker.h>

namespace gpio_test {

void GpioTest::DdkRelease() {
  done_ = true;
  thrd_join(output_thread_, nullptr);
  thrd_join(interrupt_thread_, nullptr);

  fidl::WireResult result = gpios_[GPIO_BUTTON]->ReleaseInterrupt();
  if (!result.ok()) {
    zxlogf(ERROR, "Failed to send ReleaseInterrupt request: %s", result.status_string());
  } else if (result->is_error()) {
    zxlogf(ERROR, "Failed to release interrupt: %s", zx_status_get_string(result->error_value()));
  }

  delete this;
}

// test thread that cycles all of the GPIOs provided to us
int GpioTest::OutputThread() {
  for (uint32_t i = 0; i < gpio_count_ - 1; i++) {
    fidl::WireResult result = gpios_[i]->ConfigOut(0);
    if (!result.ok()) {
      zxlogf(ERROR, "Failed to send ConfigOut request to gpio %u: %s", i, result.status_string());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "Failed to configure gpio %u to output: %s", i,
             zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }

  while (!done_) {
    // Assuming here that the last GPIO is the input button
    // so we don't toggle that one
    for (uint32_t i = 0; i < gpio_count_ - 1; i++) {
      {
        fidl::WireResult result = gpios_[i]->Write(1);
        if (!result.ok()) {
          zxlogf(ERROR, "Failed to send Write request to gpio %u: %s", i, result.status_string());
          return result.status();
        }
        if (result->is_error()) {
          zxlogf(ERROR, "Failed to write to gpio %u: %s", i,
                 zx_status_get_string(result->error_value()));
          return result->error_value();
        }
      }
      sleep(1);
      {
        fidl::WireResult result = gpios_[i]->Write(0);
        if (!result.ok()) {
          zxlogf(ERROR, "Failed to send Write request to gpio %u: %s", i, result.status_string());
          return result.status();
        }
        if (result->is_error()) {
          zxlogf(ERROR, "Failed to write to gpio %u: %s", i,
                 zx_status_get_string(result->error_value()));
          return result->error_value();
        }
      }
      sleep(1);
    }
  }

  return 0;
}

// test thread that cycles runs tests for GPIO interrupts
zx_status_t GpioTest::InterruptThread() {
  {
    fidl::WireResult result =
        gpios_[GPIO_BUTTON]->ConfigIn(fuchsia_hardware_gpio::GpioFlags::kPullDown);
    if (!result.ok()) {
      zxlogf(ERROR, "Failed to send ConfigIn request to gpio %u: %s", GPIO_BUTTON,
             result.status_string());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "Failed to configure gpio %u to input: %s", GPIO_BUTTON,
             zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }

  fidl::WireResult interrupt_result =
      gpios_[GPIO_BUTTON]->GetInterrupt(ZX_INTERRUPT_MODE_EDGE_HIGH);
  if (!interrupt_result.ok()) {
    zxlogf(ERROR, "Failed to send GetInterrupt request to gpio %u: %s", GPIO_BUTTON,
           interrupt_result.status_string());
    return interrupt_result.status();
  }
  if (interrupt_result->is_error()) {
    zxlogf(ERROR, "Failed to get interrupt from gpio %u: %s", GPIO_BUTTON,
           zx_status_get_string(interrupt_result->error_value()));
    return interrupt_result->error_value();
  }
  interrupt_ = std::move(interrupt_result.value()->irq);

  while (!done_) {
    zxlogf(INFO, "Waiting for GPIO Test Input Interrupt");
    auto status = interrupt_.wait(nullptr);
    if (status != ZX_OK) {
      zxlogf(ERROR, "%s: interrupt wait failed %d", __func__, status);
      return status;
    }
    zxlogf(INFO, "Received GPIO Test Input Interrupt");
    fidl::WireResult read_result = gpios_[GPIO_LED]->Read();
    if (!read_result.ok()) {
      zxlogf(ERROR, "Failed to send Read request to gpio %u: %s", GPIO_LED,
             read_result.status_string());
      return read_result.status();
    }
    if (read_result->is_error()) {
      zxlogf(ERROR, "Failed to read gpio %u: %s", GPIO_LED,
             zx_status_get_string(read_result->error_value()));
      return read_result->error_value();
    }
    {
      fidl::WireResult result = gpios_[GPIO_LED]->Write(!read_result.value()->value);
      if (!result.ok()) {
        zxlogf(ERROR, "Failed to send Write request to gpio %u: %s", GPIO_LED,
               result.status_string());
        return result.status();
      }
      if (result->is_error()) {
        zxlogf(ERROR, "Failed to write to gpio %u: %s", GPIO_LED,
               zx_status_get_string(result->error_value()));
        return result->error_value();
      }
    }
    sleep(1);
  }

  return ZX_OK;
}

zx_status_t GpioTest::Create(void* ctx, zx_device_t* parent) {
  fbl::AllocChecker ac;
  auto dev = std::unique_ptr<GpioTest>(new (&ac) GpioTest(parent));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  auto status = dev->Init();
  if (status != ZX_OK) {
    return status;
  }

  // devmgr is now in charge of the device.
  [[maybe_unused]] auto* dummy = dev.release();
  return ZX_OK;
}

zx_status_t GpioTest::Init() {
  gpio_count_ = DdkGetFragmentCount();

  fbl::AllocChecker ac;
  gpios_ = fbl::Array(new (&ac) fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio>[gpio_count_],
                      gpio_count_);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  composite_device_fragment_t fragments[gpio_count_];
  size_t actual;
  DdkGetFragments(fragments, gpio_count_, &actual);
  if (actual != gpio_count_) {
    return ZX_ERR_INTERNAL;
  }

  for (uint32_t i = 0; i < gpio_count_; i++) {
    zx::result client = DdkConnectFragmentFidlProtocol<fuchsia_hardware_gpio::Service::Device>(
        parent(), fragments[i].name);
    if (client.is_error()) {
      zxlogf(ERROR, "Failed to get gpio protocol from fragment %s: %s", fragments[i].name,
             client.status_string());
      return client.status_value();
    }
    gpios_[i].Bind(std::move(client.value()));
  }

  auto status = DdkAdd("gpio-test", DEVICE_ADD_NON_BINDABLE);
  if (status != ZX_OK) {
    return status;
  }

  thrd_create_with_name(
      &output_thread_,
      [](void* arg) -> int { return reinterpret_cast<GpioTest*>(arg)->OutputThread(); }, this,
      "gpio-test output");
  thrd_create_with_name(
      &interrupt_thread_,
      [](void* arg) -> int { return reinterpret_cast<GpioTest*>(arg)->InterruptThread(); }, this,
      "gpio-test interrupt");

  return ZX_OK;
}

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = GpioTest::Create;
  return ops;
}();

}  // namespace gpio_test

ZIRCON_DRIVER(gpio_test, gpio_test::driver_ops, "zircon", "0.1");
