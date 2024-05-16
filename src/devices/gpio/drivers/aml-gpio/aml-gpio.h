// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_GPIO_DRIVERS_AML_GPIO_AML_GPIO_H_
#define SRC_DEVICES_GPIO_DRIVERS_AML_GPIO_AML_GPIO_H_

#include <fidl/fuchsia.hardware.gpioimpl/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.device/cpp/fidl.h>
#include <lib/async/cpp/executor.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/fpromise/promise.h>
#include <lib/mmio/mmio.h>
#include <lib/stdcompat/span.h>

#include <cstdint>

#include <fbl/array.h>

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

class AmlGpio : public fdf::WireServer<fuchsia_hardware_gpioimpl::GpioImpl> {
 public:
  AmlGpio(fidl::ClientEnd<fuchsia_hardware_platform_device::Device> pdev, fdf::MmioBuffer mmio_gpio,
          fdf::MmioBuffer mmio_gpio_ao, fdf::MmioBuffer mmio_interrupt,
          cpp20::span<const AmlGpioBlock> gpio_blocks, const AmlGpioInterrupt* gpio_interrupt,
          uint32_t pid, fbl::Array<uint16_t> irq_info)
      : pdev_(std::move(pdev), fdf::Dispatcher::GetCurrent()->async_dispatcher()),
        mmios_{std::move(mmio_gpio), std::move(mmio_gpio_ao)},
        mmio_interrupt_(std::move(mmio_interrupt)),
        gpio_blocks_(gpio_blocks),
        gpio_interrupt_(gpio_interrupt),
        pid_(pid),
        irq_info_(std::move(irq_info)) {}

 private:
  friend class AmlGpioDriver;

  fidl::ProtocolHandler<fuchsia_hardware_gpioimpl::GpioImpl> CreateHandler() {
    return bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->get(),
                                   fidl::kIgnoreBindingClosure);
  }

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_gpioimpl::GpioImpl> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {
    FDF_LOG(ERROR, "Unexpected gpioimpl FIDL call: 0x%lx", metadata.method_ordinal);
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

  fidl::WireClient<fuchsia_hardware_platform_device::Device> pdev_;
  std::array<fdf::MmioBuffer, 2> mmios_;  // separate MMIO for AO domain
  fdf::MmioBuffer mmio_interrupt_;
  const cpp20::span<const AmlGpioBlock> gpio_blocks_;
  const AmlGpioInterrupt* gpio_interrupt_;
  const uint32_t pid_;
  fbl::Array<uint16_t> irq_info_;
  uint8_t irq_status_{};
  fdf::ServerBindingGroup<fuchsia_hardware_gpioimpl::GpioImpl> bindings_;
};

class AmlGpioDriver : public fdf::DriverBase {
 public:
  AmlGpioDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase("aml-gpio", std::move(start_args), std::move(dispatcher)),
        executor_(fdf::DriverBase::dispatcher()) {}

  void Start(fdf::StartCompleter completer) override;

 protected:
  // MapMmio can be overridden by a test in order to provide an fdf::MmioBuffer backed by a fake.
  virtual fpromise::promise<fdf::MmioBuffer, zx_status_t> MapMmio(
      fidl::WireClient<fuchsia_hardware_platform_device::Device>& pdev, uint32_t mmio_id);

 private:
  void OnCompatServerInitialized(fdf::StartCompleter completer);
  void OnGetNodeDeviceInfo(const fuchsia_hardware_platform_device::wire::NodeDeviceInfo& info,
                           fdf::StartCompleter completer);
  void OnGetBoardInfo(const fuchsia_hardware_platform_device::wire::BoardInfo& board_info,
                      uint32_t irq_count, fdf::StartCompleter completer);
  void MapMmios(uint32_t pid, uint32_t irq_count, fdf::StartCompleter completer);
  void AddNode(uint32_t pid, uint32_t irq_count, std::vector<fdf::MmioBuffer> mmios,
               fdf::StartCompleter completer);

  fidl::WireClient<fuchsia_driver_framework::Node> parent_;
  fidl::WireClient<fuchsia_driver_framework::NodeController> controller_;
  fidl::WireClient<fuchsia_hardware_platform_device::Device> pdev_;
  compat::AsyncInitializedDeviceServer compat_server_;
  std::unique_ptr<AmlGpio> device_;
  async::Executor executor_;
};

}  // namespace gpio

#endif  // SRC_DEVICES_GPIO_DRIVERS_AML_GPIO_AML_GPIO_H_
