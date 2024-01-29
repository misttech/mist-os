// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_VIM3_USB_PHY_VIM3_USB_PHY_H_
#define SRC_DEVICES_USB_DRIVERS_VIM3_USB_PHY_VIM3_USB_PHY_H_

#include <fidl/fuchsia.hardware.registers/cpp/wire.h>
#include <fidl/fuchsia.hardware.usb.phy/cpp/driver/fidl.h>
#include <lib/async/cpp/irq.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/mmio/mmio.h>
#include <lib/zx/interrupt.h>

#include <fbl/auto_lock.h>
#include <fbl/mutex.h>
#include <usb/usb.h>

namespace vim3_usb_phy {

class Vim3UsbPhyDevice;

// This is the main class for the platform bus driver.
// The Vim3UsbPhy driver manages 3 different controllers:
//  - One USB 2.0 controller that is only supports host mode.
//  - One USB 2.0 controller that supports OTG (both host and device mode).
//  - One USB 3.0 controller that only supports host mode.
class Vim3UsbPhy : public fdf::Server<fuchsia_hardware_usb_phy::UsbPhy> {
 public:
  // Public for testing.
  enum class UsbMode {
    UNKNOWN,
    HOST,
    PERIPHERAL,
  };

  class UsbPhy2 {
   public:
    UsbPhy2(uint8_t idx, fdf::MmioBuffer mmio, bool is_otg_capable, usb_mode_t dr_mode)
        : idx_(idx), mmio_(std::move(mmio)), is_otg_capable_(is_otg_capable), dr_mode_(dr_mode) {}

    void SetMode(UsbMode mode, Vim3UsbPhy* device);  // Must hold parent's lock_ while calling.

    uint8_t idx() const { return idx_; }
    fdf::MmioBuffer& mmio() { return mmio_; }
    bool is_otg_capable() const { return is_otg_capable_; }
    usb_mode_t dr_mode() const { return dr_mode_; }

    // Public for testing.
    UsbMode mode() { return phy_mode_; }

   private:
    const uint8_t idx_;  // For indexing into usbctrl_mmio.
    fdf::MmioBuffer mmio_;
    const bool is_otg_capable_;
    const usb_mode_t dr_mode_;  // USB Controller Mode. Internal to Driver.

    UsbMode phy_mode_ =
        UsbMode::UNKNOWN;  // Physical USB mode. Must hold parent's lock_ while accessing.
  };

  Vim3UsbPhy(Vim3UsbPhyDevice* controller,
             fidl::ClientEnd<fuchsia_hardware_registers::Device> reset_register,
             std::array<uint32_t, 8> pll_settings, fdf::MmioBuffer usbctrl_mmio, UsbPhy2 usbphy20,
             UsbPhy2 usbphy21, fdf::MmioBuffer usbphy3_mmio, zx::interrupt irq)
      : controller_(controller),
        reset_register_(std::move(reset_register)),
        usbctrl_mmio_(std::move(usbctrl_mmio)),
        usbphy2_({std::move(usbphy20), std::move(usbphy21)}),
        usbphy3_mmio_(std::move(usbphy3_mmio)),
        irq_(std::move(irq)),
        pll_settings_(pll_settings) {}
  ~Vim3UsbPhy() override {
    irq_handler_.Cancel();
    irq_.destroy();
  }

  zx_status_t Init();

  // fuchsia_hardware_usb_phy::UsbPhy required methods
  void ConnectStatusChanged(ConnectStatusChangedRequest& request,
                            ConnectStatusChangedCompleter::Sync& completer) override;
  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_hardware_usb_phy::UsbPhy> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {
    FDF_LOG(ERROR, "Unknown method %lu", metadata.method_ordinal);
  }

 private:
  DISALLOW_COPY_ASSIGN_AND_MOVE(Vim3UsbPhy);

  void InitPll(fdf::MmioBuffer* mmio);
  zx_status_t InitPhy();
  zx_status_t InitPhy3();
  zx_status_t InitOtg();

  zx_status_t CrBusAddr(uint32_t addr);
  uint32_t CrBusRead(uint32_t addr);
  zx_status_t CrBusWrite(uint32_t addr, uint32_t data);

  void HandleIrq(async_dispatcher_t* dispatcher, async::IrqBase* irq, zx_status_t status,
                 const zx_packet_interrupt_t* interrupt);

  Vim3UsbPhyDevice* controller_;

  fidl::WireSyncClient<fuchsia_hardware_registers::Device> reset_register_;
  fdf::MmioBuffer usbctrl_mmio_;
  std::array<UsbPhy2, 2> usbphy2_;
  fdf::MmioBuffer usbphy3_mmio_;

  zx::interrupt irq_;
  async::IrqMethod<Vim3UsbPhy, &Vim3UsbPhy::HandleIrq> irq_handler_{this};

  fbl::Mutex lock_;

  // Magic numbers for PLL from metadata
  const std::array<uint32_t, 8> pll_settings_;

  bool dwc2_connected_ = false;
};

class Vim3UsbPhyDevice : public fdf::DriverBase {
 private:
  static constexpr char kDeviceName[] = "vim3_usb_phy";

 public:
  Vim3UsbPhyDevice(fdf::DriverStartArgs start_args,
                   fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : fdf::DriverBase(kDeviceName, std::move(start_args), std::move(driver_dispatcher)) {}
  zx::result<> Start() override;
  void Stop() override;

  zx::result<> AddXhci() { return AddDevice(xhci_); }
  zx::result<> RemoveXhci() { return RemoveDevice(xhci_); }
  zx::result<> AddDwc2() { return AddDevice(dwc2_); }
  zx::result<> RemoveDwc2() { return RemoveDevice(dwc2_); }

 private:
  friend class Vim3UsbPhyTest;

  zx::result<> CreateNode();

  struct ChildNode {
    explicit ChildNode(std::string_view name, uint32_t property_did)
        : name_(name), property_did_(property_did) {}

    void reset() __TA_REQUIRES(lock_) {
      count_ = 0;
      if (controller_) {
        auto result = controller_->Remove();
        if (!result.ok()) {
          FDF_LOG(ERROR, "Failed to remove %s. %s", name_.data(),
                  result.FormatDescription().c_str());
        }
        controller_.TakeClientEnd().reset();
      }
      compat_server_.reset();
    }

    const std::string_view name_;
    const uint32_t property_did_;

    fbl::Mutex lock_;
    fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_ __TA_GUARDED(lock_);
    compat::SyncInitializedDeviceServer compat_server_ __TA_GUARDED(lock_);
    std::atomic_uint32_t count_ __TA_GUARDED(lock_) = 0;
  };
  zx::result<> AddDevice(ChildNode& node);
  zx::result<> RemoveDevice(ChildNode& node);

  std::unique_ptr<Vim3UsbPhy> device_;

  fdf::ServerBindingGroup<fuchsia_hardware_usb_phy::UsbPhy> bindings_;
  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;

  ChildNode xhci_{"xhci", PDEV_DID_USB_XHCI_COMPOSITE};
  ChildNode dwc2_{"dwc2", PDEV_DID_USB_DWC2};
};

}  // namespace vim3_usb_phy

#endif  // SRC_DEVICES_USB_DRIVERS_VIM3_USB_PHY_VIM3_USB_PHY_H_
