// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_GPIO_DRIVERS_AML_GPIO_AML_GPIO_H_
#define SRC_DEVICES_GPIO_DRIVERS_AML_GPIO_AML_GPIO_H_

#include <fidl/fuchsia.hardware.gpioimpl/cpp/driver/fidl.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/mmio/mmio.h>

#include <array>
#include <cstdint>

#include <ddktl/device.h>
#include <fbl/array.h>

#include "sdk/lib/driver/outgoing/cpp/outgoing_directory.h"

namespace gpio {

struct AmlGpioBlock {
  uint32_t start_pin;
  uint32_t pin_block;
  uint32_t pin_count;
  uint32_t mux_offset;
  uint32_t oen_offset;
  uint32_t input_offset;
  uint32_t output_offset;
  uint32_t output_shift;  // Used for GPIOAO block
  uint32_t pull_offset;
  uint32_t pull_en_offset;
  uint32_t mmio_index;
  uint32_t pin_start;
  uint32_t ds_offset;
};

struct AmlGpioInterrupt {
  uint32_t pin_select_offset;
  uint32_t edge_polarity_offset;
  uint32_t filter_select_offset;
};

class AmlGpio;
using DeviceType = ddk::Device<AmlGpio>;

class AmlGpio : public DeviceType, public fdf::WireServer<fuchsia_hardware_gpioimpl::GpioImpl> {
 public:
  static zx_status_t Create(void* ctx, zx_device_t* parent);

  void DdkRelease() { delete this; }

 protected:
  // for AmlGpioTest
  explicit AmlGpio(ddk::PDevFidl pdev, fdf::MmioBuffer mmio_gpio, fdf::MmioBuffer mmio_gpio_a0,
                   fdf::MmioBuffer mmio_interrupt, const AmlGpioBlock* gpio_blocks,
                   const AmlGpioInterrupt* gpio_interrupt, size_t block_count,
                   pdev_device_info_t info, fbl::Array<uint16_t> irq_info,
                   fdf_dispatcher_t* dispatcher)
      : DeviceType(nullptr),
        pdev_(std::move(pdev)),
        mmios_{std::move(mmio_gpio), std::move(mmio_gpio_a0)},
        mmio_interrupt_(std::move(mmio_interrupt)),
        gpio_blocks_(gpio_blocks),
        gpio_interrupt_(gpio_interrupt),
        block_count_(block_count),
        info_(std::move(info)),
        irq_info_(std::move(irq_info)),
        irq_status_(0),
        outgoing_(dispatcher) {}

 private:
  explicit AmlGpio(zx_device_t* parent, fdf::MmioBuffer mmio_gpio, fdf::MmioBuffer mmio_gpio_a0,
                   fdf::MmioBuffer mmio_interrupt, const AmlGpioBlock* gpio_blocks,
                   const AmlGpioInterrupt* gpio_interrupt, size_t block_count,
                   pdev_device_info_t info, fbl::Array<uint16_t> irq_info)
      : DeviceType(parent),
        pdev_(parent),
        mmios_{std::move(mmio_gpio), std::move(mmio_gpio_a0)},
        mmio_interrupt_(std::move(mmio_interrupt)),
        gpio_blocks_(gpio_blocks),
        gpio_interrupt_(gpio_interrupt),
        block_count_(block_count),
        info_(std::move(info)),
        irq_info_(std::move(irq_info)),
        irq_status_(0),
        outgoing_(fdf::Dispatcher::GetCurrent()->get()) {}

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_gpioimpl::GpioImpl> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {
    zxlogf(ERROR, "Unexpected gpioimpl FIDL call: 0x%lx", metadata.method_ordinal);
  }

  void ConfigIn(fuchsia_hardware_gpioimpl::wire::GpioImplConfigInRequest* request,
                fdf::Arena& arena, ConfigInCompleter::Sync& completer) override;
  void ConfigOut(fuchsia_hardware_gpioimpl::wire::GpioImplConfigOutRequest* request,
                 fdf::Arena& arena, ConfigOutCompleter::Sync& completer) override;
  void SetAltFunction(fuchsia_hardware_gpioimpl::wire::GpioImplSetAltFunctionRequest* request,
                      fdf::Arena& arena, SetAltFunctionCompleter::Sync& completer) override;
  void Read(fuchsia_hardware_gpioimpl::wire::GpioImplReadRequest* request, fdf::Arena& arena,
            ReadCompleter::Sync& completer) override;
  void Write(fuchsia_hardware_gpioimpl::wire::GpioImplWriteRequest* request, fdf::Arena& arena,
             WriteCompleter::Sync& completer) override;
  void SetPolarity(fuchsia_hardware_gpioimpl::wire::GpioImplSetPolarityRequest* request,
                   fdf::Arena& arena, SetPolarityCompleter::Sync& completer) override;
  void SetDriveStrength(fuchsia_hardware_gpioimpl::wire::GpioImplSetDriveStrengthRequest* request,
                        fdf::Arena& arena, SetDriveStrengthCompleter::Sync& completer) override;
  void GetDriveStrength(fuchsia_hardware_gpioimpl::wire::GpioImplGetDriveStrengthRequest* request,
                        fdf::Arena& arena, GetDriveStrengthCompleter::Sync& completer) override;
  void GetInterrupt(fuchsia_hardware_gpioimpl::wire::GpioImplGetInterruptRequest* request,
                    fdf::Arena& arena, GetInterruptCompleter::Sync& completer) override;
  void ReleaseInterrupt(fuchsia_hardware_gpioimpl::wire::GpioImplReleaseInterruptRequest* request,
                        fdf::Arena& arena, ReleaseInterruptCompleter::Sync& completer) override;
  void GetPins(fdf::Arena& arena, GetPinsCompleter::Sync& completer) override;
  void GetInitSteps(fdf::Arena& arena, GetInitStepsCompleter::Sync& completer) override;
  void GetControllerId(fdf::Arena& arena, GetControllerIdCompleter::Sync& completer) override;

  zx_status_t AmlPinToBlock(uint32_t pin, const AmlGpioBlock** out_block,
                            uint32_t* out_pin_index) const;

  ddk::PDevFidl pdev_;
  std::array<fdf::MmioBuffer, 2> mmios_;  // separate MMIO for AO domain
  fdf::MmioBuffer mmio_interrupt_;
  const AmlGpioBlock* gpio_blocks_;
  const AmlGpioInterrupt* gpio_interrupt_;
  size_t block_count_;
  const pdev_device_info_t info_;
  fbl::Array<uint16_t> irq_info_;
  uint8_t irq_status_;
  fdf::ServerBindingGroup<fuchsia_hardware_gpioimpl::GpioImpl> bindings_;
  fdf::OutgoingDirectory outgoing_;
};

}  // namespace gpio

#endif  // SRC_DEVICES_GPIO_DRIVERS_AML_GPIO_AML_GPIO_H_
