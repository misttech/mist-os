// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_A1_USB_PHY_A1_USB_PHY_H_
#define SRC_DEVICES_USB_DRIVERS_A1_USB_PHY_A1_USB_PHY_H_

#include <fuchsia/hardware/usb/modeswitch/cpp/banjo.h>
#include <fuchsia/hardware/usb/phy/cpp/banjo.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/mmio/mmio.h>
#include <lib/zx/interrupt.h>
#include <threads.h>

#include <ddktl/device.h>
#include <fbl/auto_lock.h>
#include <fbl/mutex.h>
#include <soc/aml-common/aml-registers.h>

#include "src/devices/usb/drivers/a1-usb-phy/dwc2-device.h"
#include "src/devices/usb/drivers/a1-usb-phy/xhci-device.h"

namespace a1_usb_phy {

class A1UsbPhy;
using A1UsbPhyType =
    ddk::Device<A1UsbPhy, ddk::Initializable, ddk::Unbindable, ddk::ChildPreReleaseable>;

// This is the main class for the platform bus driver.
class A1UsbPhy : public A1UsbPhyType, public ddk::UsbPhyProtocol<A1UsbPhy, ddk::base_protocol> {
 public:
  // Public for testing.
  enum class UsbMode {
    UNKNOWN,
    HOST,
    PERIPHERAL,
  };

  explicit A1UsbPhy(zx_device_t* parent) : A1UsbPhyType(parent), pdev_(parent) {}

  static zx_status_t Create(void* ctx, zx_device_t* parent);

  // USB PHY protocol implementation.
  void UsbPhyConnectStatusChanged(bool connected);

  // Device protocol implementation.
  void DdkInit(ddk::InitTxn txn);
  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkChildPreRelease(void* child_ctx);
  void DdkRelease();

  // Public for testing.
  UsbMode mode() {
    fbl::AutoLock lock(&lock_);
    return phy_mode_;
  }

 private:
  DISALLOW_COPY_ASSIGN_AND_MOVE(A1UsbPhy);

  void InitPll(fdf::MmioBuffer* mmio);
  zx_status_t InitPhy();
  zx_status_t InitOtg();

  // Called when |SetMode| completes.
  using SetModeCompletion = fit::callback<void(void)>;
  void SetMode(UsbMode mode, SetModeCompletion completion) __TA_REQUIRES(lock_);

  zx_status_t Init();
  void InitUsbClk();
  zx_status_t UsbPowerOn();

  ddk::PDev pdev_;
  std::optional<fdf::MmioBuffer> usbctrl_mmio_;
  std::optional<fdf::MmioBuffer> usbphy_mmio_;
  std::optional<fdf::MmioBuffer> reset_mmio_;
  std::optional<fdf::MmioBuffer> clk_mmio_;

  // Control PowerDomain
  zx::resource smc_monitor_;

  fbl::Mutex lock_;

  // Magic numbers for PLL from metadata
  uint32_t pll_settings_[8];

  UsbMode phy_mode_ __TA_GUARDED(lock_) = UsbMode::UNKNOWN;  // Physical USB mode.
  usb_mode_t dr_mode_ = USB_MODE_OTG;  // USB Controller Mode. Internal to Driver.
  bool dwc2_connected_ = false;
};

}  // namespace a1_usb_phy

#endif  // SRC_DEVICES_USB_DRIVERS_A1_USB_PHY_A1_USB_PHY_H_
