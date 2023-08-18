// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_AML_USB_PHY_V2_AML_USB_PHY_H_
#define SRC_DEVICES_USB_DRIVERS_AML_USB_PHY_V2_AML_USB_PHY_H_

#include <fidl/fuchsia.hardware.registers/cpp/wire.h>
#include <fuchsia/hardware/usb/phy/cpp/banjo.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/mmio/mmio.h>
#include <lib/sync/cpp/completion.h>
#include <lib/zx/interrupt.h>
#include <threads.h>

#include <ddktl/device.h>
#include <fbl/auto_lock.h>
#include <fbl/mutex.h>
#include <soc/aml-common/aml-registers.h>
#include <usb/usb.h>

namespace aml_usb_phy {

class AmlUsbPhy;
using AmlUsbPhyType =
    ddk::Device<AmlUsbPhy, ddk::Initializable, ddk::Unbindable, ddk::ChildPreReleaseable>;

// This is the main class for the platform bus driver.
class AmlUsbPhy : public AmlUsbPhyType, public ddk::UsbPhyProtocol<AmlUsbPhy, ddk::base_protocol> {
 public:
  // Public for testing.
  enum class UsbMode {
    UNKNOWN,
    HOST,
    PERIPHERAL,
  };

  explicit AmlUsbPhy(zx_device_t* parent) : AmlUsbPhyType(parent) {}

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
  ddk::PDevFidl& pdev() { return pdev_; }

  // Testing seam (see below)
  libsync::Completion& testing_edge() { return testing_edge_; }

 private:
  DISALLOW_COPY_ASSIGN_AND_MOVE(AmlUsbPhy);

  fdf::MmioBuffer* ctrl_mmio() { return &usbctrl_mmio_.value(); }
  fdf::MmioBuffer* phy20_mmio() { return &usbphy20_mmio_.value(); }
  fdf::MmioBuffer* phy21_mmio() { return &usbphy21_mmio_.value(); }

  void InitPll(fdf::MmioBuffer* mmio);
  zx_status_t InitPhy();
  zx_status_t InitOtg();

  // Called when |SetMode| completes.
  using SetModeCompletion = fit::callback<void(void)>;
  void ReadOtgAndSetMode(SetModeCompletion completion = []() { /* no-op */ }) __TA_REQUIRES(lock_);

  void SetMode(
      UsbMode mode, SetModeCompletion completion = []() { /* no-op */ }) __TA_REQUIRES(lock_);

  zx_status_t AddXhciDevice() __TA_REQUIRES(lock_);
  void RemoveXhciDevice(SetModeCompletion completion) __TA_REQUIRES(lock_);
  zx_status_t AddDwc2Device() __TA_REQUIRES(lock_);
  void RemoveDwc2Device(SetModeCompletion completion) __TA_REQUIRES(lock_);

  zx_status_t Init();
  int IrqThread();

  ddk::PDevFidl pdev_;
  fidl::WireSyncClient<fuchsia_hardware_registers::Device> reset_register_;
  std::optional<fdf::MmioBuffer> usbctrl_mmio_;
  std::optional<fdf::MmioBuffer> usbphy20_mmio_;
  std::optional<fdf::MmioBuffer> usbphy21_mmio_;

  zx::interrupt irq_;
  thrd_t irq_thread_;
  std::atomic_bool irq_thread_started_ = false;

  fbl::Mutex lock_;

  // Magic numbers for PLL from metadata
  uint32_t pll_settings_[8];

  // Device nodes for child devices. The resources pointed at are managed by the DDK.
  zx_device_t* xhci_device_ __TA_GUARDED(lock_) = nullptr;
  zx_device_t* dwc2_device_ __TA_GUARDED(lock_) = nullptr;

  UsbMode phy_mode_ __TA_GUARDED(lock_) = UsbMode::UNKNOWN;  // Physical USB mode.
  usb_mode_t dr_mode_ = USB_MODE_OTG;  // USB Controller Mode. Internal to Driver.
  bool dwc2_connected_ = false;

  // If set, indicates that the device has a pending SetMode which
  // will be completed once |DdkChildPreRelease| is called.
  SetModeCompletion set_mode_completion_ __TA_GUARDED(lock_);

  // This completion is a testing seam, and only used to synchronize tests to the side effects of
  // the async. irq thread.
  //
  // To use (from the perspective of the test):
  //   1. Ensure the completion is in an unsignaled state.
  //   2. Do anything that would unblock the irq thread (such as signaling irq_).
  //   3. Invoke Wait() on the completion.
  //   4. When Wait() returns, invoke WaitUntilAsyncRemoveCalled().
  //   5. When WaitUntilAsyncRemovedCalled() returns, invoke ReleaseFlaggedDevices().
  //   6. (optional) reset the completion if the test has more work to perform.
  //
  // This will ensure the test is blocked until the irq thread performs the work of SetMode(). When
  // Wait() returns, the correct mode has been configured, the requisite device has been added, and
  // any applicable prior device has been DdkAsyncRemoved. If a child device was removed, the irq
  // thread will block and wait for the Ddk to (asynchronously) release the child. As the child is
  // released, the Ddk will invoke ChildPreRelease() - at which time the irq thread's completion
  // callback will be invoked.
  //
  // When running under mock-ddk, it is the test's responsibility to perform the work of the Ddk.
  // That's done by way of waiting until DdkAsyncRemove() was invoked (if applicable), and then
  // calling ReleaseFlaggedDevices(), which has the side effect of unblocking the irq thread.
  libsync::Completion testing_edge_;
};

}  // namespace aml_usb_phy

#endif  // SRC_DEVICES_USB_DRIVERS_AML_USB_PHY_V2_AML_USB_PHY_H_
