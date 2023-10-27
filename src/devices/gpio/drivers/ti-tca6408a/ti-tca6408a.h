// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_GPIO_DRIVERS_TI_TCA6408A_TI_TCA6408A_H_
#define SRC_DEVICES_GPIO_DRIVERS_TI_TCA6408A_TI_TCA6408A_H_

#include <fidl/fuchsia.hardware.gpioimpl/cpp/driver/fidl.h>
#include <lib/ddk/device.h>
#include <lib/device-protocol/i2c-channel.h>
#include <lib/zx/result.h>

#include <ddktl/device.h>
#include <ddktl/protocol/empty-protocol.h>

namespace gpio {

class TiTca6408a;
using DeviceType = ddk::Device<TiTca6408a>;

class TiTca6408aTest;

class TiTca6408a : public DeviceType,
                   public fdf::Server<fuchsia_hardware_gpioimpl::GpioImpl>,
                   public ddk::EmptyProtocol<ZX_PROTOCOL_GPIO_IMPL> {
 public:
  static zx_status_t Create(void* ctx, zx_device_t* parent);

  TiTca6408a(zx_device_t* parent, ddk::I2cChannel i2c, uint32_t pin_index_offset)
      : DeviceType(parent), i2c_(std::move(i2c)), pin_index_offset_(pin_index_offset) {}

  void DdkRelease() { delete this; }

  void ConfigIn(ConfigInRequest& request, ConfigInCompleter::Sync& completer) override;
  void ConfigOut(ConfigOutRequest& request, ConfigOutCompleter::Sync& completer) override;
  void SetAltFunction(SetAltFunctionRequest& request,
                      SetAltFunctionCompleter::Sync& completer) override;
  void Read(ReadRequest& request, ReadCompleter::Sync& completer) override;
  void Write(WriteRequest& request, WriteCompleter::Sync& completer) override;
  void SetPolarity(SetPolarityRequest& request, SetPolarityCompleter::Sync& completer) override;
  void SetDriveStrength(SetDriveStrengthRequest& request,
                        SetDriveStrengthCompleter::Sync& completer) override;
  void GetDriveStrength(GetDriveStrengthRequest& request,
                        GetDriveStrengthCompleter::Sync& completer) override;
  void GetInterrupt(GetInterruptRequest& request, GetInterruptCompleter::Sync& completer) override;
  void ReleaseInterrupt(ReleaseInterruptRequest& request,
                        ReleaseInterruptCompleter::Sync& completer) override;

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_gpioimpl::GpioImpl> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {
    zxlogf(ERROR, "Unknown method %lu", metadata.method_ordinal);
  }

 protected:
  friend class TiTca6408aTest;

 private:
  static constexpr uint32_t kPinCount = 8;

  enum class Register : uint8_t {
    kInputPort = 0,
    kOutputPort = 1,
    kPolarityInversion = 2,
    kConfiguration = 3,
  };

  zx_status_t Write(uint32_t index, uint8_t value);

  bool IsIndexInRange(uint32_t index) const {
    return index >= pin_index_offset_ && index < (pin_index_offset_ + kPinCount);
  }

  zx::result<uint8_t> ReadBit(Register reg, uint32_t index);
  zx::result<> SetBit(Register reg, uint32_t index);
  zx::result<> ClearBit(Register reg, uint32_t index);

  ddk::I2cChannel i2c_;
  const uint32_t pin_index_offset_;
};

}  // namespace gpio

#endif  // SRC_DEVICES_GPIO_DRIVERS_TI_TCA6408A_TI_TCA6408A_H_
