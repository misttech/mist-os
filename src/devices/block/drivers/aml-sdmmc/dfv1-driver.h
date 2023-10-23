// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_AML_SDMMC_DFV1_DRIVER_H_
#define SRC_DEVICES_BLOCK_DRIVERS_AML_SDMMC_DFV1_DRIVER_H_

#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/mmio/mmio.h>
#include <lib/zx/interrupt.h>

#include <ddktl/device.h>

#include "aml-sdmmc.h"
#include "io-buffer.h"

namespace aml_sdmmc {

class Dfv1Driver;
using Dfv1DriverType = ddk::Device<Dfv1Driver, ddk::Suspendable>;

class Dfv1Driver : public Dfv1DriverType, public AmlSdmmc {
 public:
  static zx_status_t Create(void* ctx, zx_device_t* parent);

  Dfv1Driver(zx_device_t* parent, zx::bti bti, fdf::MmioBuffer mmio, aml_sdmmc_config_t config,
             zx::interrupt irq, fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> reset_gpio,
             aml_sdmmc::IoBuffer descs_buffer)
      : Dfv1DriverType(parent),
        AmlSdmmc(std::move(bti), std::move(mmio), config, std::move(irq), std::move(reset_gpio),
                 std::move(descs_buffer)),
        dispatcher_(fdf::Dispatcher::GetCurrent()->get()),
        outgoing_dir_(fdf::OutgoingDirectory::Create(dispatcher_)) {}
  ~Dfv1Driver() override = default;

  // Device protocol implementation
  void DdkRelease();
  void DdkSuspend(ddk::SuspendTxn txn);

 protected:
  // Visible for tests
  zx_status_t Bind();

 private:
  // Dispatcher for being a FIDL server serving Sdmmc requests.
  fdf_dispatcher_t* dispatcher_;
  // Serves fuchsia_hardware_sdmmc::SdmmcService.
  fdf::OutgoingDirectory outgoing_dir_;
};

}  // namespace aml_sdmmc

#endif  // SRC_DEVICES_BLOCK_DRIVERS_AML_SDMMC_DFV1_DRIVER_H_
