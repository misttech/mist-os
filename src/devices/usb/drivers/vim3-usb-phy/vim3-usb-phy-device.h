// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_VIM3_USB_PHY_VIM3_USB_PHY_DEVICE_H_
#define SRC_DEVICES_USB_DRIVERS_VIM3_USB_PHY_VIM3_USB_PHY_DEVICE_H_

#include <fidl/fuchsia.hardware.usb.phy/cpp/driver/fidl.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/driver_base.h>

#include <fbl/mutex.h>

namespace vim3_usb_phy {

class Vim3UsbPhy;

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

#endif  // SRC_DEVICES_USB_DRIVERS_VIM3_USB_PHY_VIM3_USB_PHY_DEVICE_H_
