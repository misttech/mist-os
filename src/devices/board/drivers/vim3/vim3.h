// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BOARD_DRIVERS_VIM3_VIM3_H_
#define SRC_DEVICES_BOARD_DRIVERS_VIM3_VIM3_H_

#include <fidl/fuchsia.hardware.clockimpl/cpp/wire.h>
#include <fidl/fuchsia.hardware.gpioimpl/cpp/wire.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/markers.h>
#include <lib/ddk/device.h>
#include <threads.h>

#include <optional>

#include <ddktl/device.h>
#include <fbl/macros.h>
#include <soc/aml-a311d/a311d-gpio.h>
#include <soc/aml-a311d/a311d-hw.h>

namespace vim3 {

// BTI IDs for our devices
enum {
  BTI_CANVAS,
  BTI_DISPLAY,
  BTI_EMMC,
  BTI_ETHERNET,
  BTI_SD,
  BTI_SDIO,
  BTI_SYSMEM,
  BTI_NNA,
  BTI_USB,
  BTI_MALI,
  BTI_VIDEO,
  BTI_AUDIO_OUT,
  BTI_AUDIO_IN,
  BTI_AUDIO_BT_OUT,
  BTI_AUDIO_BT_IN,
  BTI_AUDIO_COMPOSITE,
};

// MAC address metadata indices.
// Currently the bootloader only sets up a single MAC zbi entry, we'll use it for both the WiFi and
// BT radio MACs.
enum {
  MACADDR_WIFI = 0,
  MACADDR_BLUETOOTH = 0,
};

class Vim3;
using Vim3Type = ddk::Device<Vim3, ddk::Initializable>;

// This is the main class for the platform bus driver.
class Vim3 : public Vim3Type {
 public:
  Vim3(zx_device_t* parent, fdf::ClientEnd<fuchsia_hardware_platform_bus::PlatformBus> pbus)
      : Vim3Type(parent), pbus_(std::move(pbus)) {}

  static zx_status_t Create(void* ctx, zx_device_t* parent);

  // Device protocol implementation.
  void DdkInit(ddk::InitTxn txn);
  void DdkRelease() {}

 private:
  DISALLOW_COPY_ASSIGN_AND_MOVE(Vim3);

  // Returns true iff the board has a MIPI-DSI display attached.
  //
  // This board driver assumes the user will not attach / detach a DSI cable
  // from a running system, since the DSI display on VIM3 is not designed to be
  // hot-pluggable. The return value of this method is constant over the
  // lifetime of the board driver.
  bool HasLcd();

  zx::result<> AdcInit();
  zx::result<> HrTimerInit();
  zx_status_t AudioInit();
  zx_status_t BacklightInit();
  zx_status_t BluetoothInit();
  zx::result<> ButtonsInit();
  zx_status_t CanvasInit();
  zx_status_t ClkInit();
  zx_status_t CpuInit();
  zx_status_t DisplayInit();
  zx_status_t EmmcInit();
  zx_status_t EthInit();
  zx_status_t GpioInit();
  zx_status_t I2cInit();
  zx_status_t PowerInit();
  zx_status_t PwmInit();
  zx_status_t RegistersInit();
  zx_status_t SdInit();
  zx_status_t SdioInit();
  zx_status_t Start();
  zx_status_t SysmemInit();
  zx_status_t TemperatureInit();
  zx_status_t TouchInit();
  zx_status_t NnaInit();
  zx_status_t UsbInit();
  zx_status_t MaliInit();
  zx_status_t VideoInit();
  zx_status_t SuspendInit();
  zx_status_t McuInit();

  int Thread();

  static fuchsia_hardware_gpioimpl::wire::InitCall GpioConfigIn(
      fuchsia_hardware_gpio::GpioFlags flags) {
    return fuchsia_hardware_gpioimpl::wire::InitCall::WithInputFlags(flags);
  }

  static fuchsia_hardware_gpioimpl::wire::InitCall GpioConfigOut(uint8_t initial_value) {
    return fuchsia_hardware_gpioimpl::wire::InitCall::WithOutputValue(initial_value);
  }

  fuchsia_hardware_gpioimpl::wire::InitCall GpioSetAltFunction(uint64_t function) {
    return fuchsia_hardware_gpioimpl::wire::InitCall::WithAltFunction(init_arena_, function);
  }

  fuchsia_hardware_gpioimpl::wire::InitCall GpioSetDriveStrength(uint64_t ds_ua) {
    return fuchsia_hardware_gpioimpl::wire::InitCall::WithDriveStrengthUa(init_arena_, ds_ua);
  }

  // TODO(https://fxbug.dev/42059490): migrate to fdf::SyncClient when it is available.
  fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus> pbus_;

  std::optional<bool> has_lcd_;

  std::optional<ddk::InitTxn> init_txn_;
  fidl::Arena<> init_arena_;
  std::vector<fuchsia_hardware_gpioimpl::wire::InitStep> gpio_init_steps_;
  std::vector<fuchsia_hardware_clockimpl::wire::InitStep> clock_init_steps_;
  thrd_t thread_;
};

class Vim3Child;
using Vim3ChildType = ddk::Device<Vim3Child>;
class Vim3Child : public Vim3ChildType {
 public:
  explicit Vim3Child(zx_device_t* parent) : Vim3ChildType(parent) {}
  void DdkRelease() {}
};

}  // namespace vim3

#endif  // SRC_DEVICES_BOARD_DRIVERS_VIM3_VIM3_H_
