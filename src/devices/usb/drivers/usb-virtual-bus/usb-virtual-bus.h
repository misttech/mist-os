// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_USB_VIRTUAL_BUS_USB_VIRTUAL_BUS_H_
#define SRC_DEVICES_USB_DRIVERS_USB_VIRTUAL_BUS_USB_VIRTUAL_BUS_H_

#include <fidl/fuchsia.hardware.usb.virtual.bus/cpp/wire.h>
#include <fuchsia/hardware/usb/bus/cpp/banjo.h>
#include <fuchsia/hardware/usb/dci/cpp/banjo.h>
#include <fuchsia/hardware/usb/hci/cpp/banjo.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/device.h>
#include <lib/sync/cpp/completion.h>
#include <threads.h>

#include <memory>
#include <optional>

#include <ddktl/device.h>
#include <fbl/condition_variable.h>
#include <fbl/mutex.h>
#include <usb/request-cpp.h>

#include "src/devices/usb/drivers/usb-virtual-bus/usb-virtual-device.h"
#include "src/devices/usb/drivers/usb-virtual-bus/usb-virtual-host.h"

namespace usb_virtual_bus {

// For mapping b_endpoint_address value to/from index in range 0 - 31.
// OUT endpoints are in range 1 - 15, IN endpoints are in range 17 - 31.
static inline uint8_t EpAddressToIndex(uint8_t addr) {
  return static_cast<uint8_t>(((addr) & 0xF) | (((addr) & 0x80) >> 3));
}

using Request = usb::BorrowedRequest<void>;
using RequestQueue = usb::BorrowedRequestQueue<void>;
using RequestVariant = std::variant<Request, usb::FidlRequest>;

// This struct represents an endpoint on the virtual device.
class UsbVirtualEp {
 public:
  ~UsbVirtualEp() {
    ZX_ASSERT(host_reqs.empty());
    ZX_ASSERT(device_reqs.is_empty());
  }

  void Init(UsbVirtualBus* bus, uint8_t index) {
    bus_ = bus;
    index_ = index;
  }

  void QueueRequest(RequestVariant request);
  zx::result<> CancelAll();
  void RequestComplete(zx_status_t status, size_t actual, RequestVariant request);

  bool is_control() const { return index_ == 0; }

  std::queue<RequestVariant> host_reqs;
  RequestQueue device_reqs;
  uint16_t max_packet_size = 0;
  bool stalled = false;

 private:
  UsbVirtualBus* bus_;
  uint8_t index_;
};

class UsbVirtualBus;
class UsbVirtualDevice;
class UsbVirtualHost;
using UsbVirtualBusType =
    ddk::Device<UsbVirtualBus, ddk::Initializable, ddk::Unbindable, ddk::ChildPreReleaseable,
                ddk::Messageable<fuchsia_hardware_usb_virtual_bus::Bus>::Mixin>;

// This is the main class for the USB virtual bus.
class UsbVirtualBus : public UsbVirtualBusType {
 public:
  explicit UsbVirtualBus(zx_device_t* parent, async_dispatcher_t* dispatcher)
      : UsbVirtualBusType(parent), dispatcher_(dispatcher), outgoing_(dispatcher) {
    for (uint8_t i = 0; i < USB_MAX_EPS; i++) {
      eps_[i].Init(this, i);
    }
  }

  static zx_status_t Create(zx_device_t* parent);

  // Device protocol implementation.
  void DdkInit(ddk::InitTxn txn);
  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkChildPreRelease(void* child_ctx);
  void DdkRelease();

  // USB device controller protocol implementation.
  void UsbDciRequestQueue(usb_request_t* usb_request,
                          const usb_request_complete_callback_t* complete_cb);
  zx_status_t UsbDciSetInterface(const usb_dci_interface_protocol_t* interface);
  zx_status_t UsbDciConfigEp(const usb_endpoint_descriptor_t* ep_desc,
                             const usb_ss_ep_comp_descriptor_t* ss_comp_desc);
  zx_status_t UsbDciDisableEp(uint8_t ep_address);
  zx_status_t UsbDciEpSetStall(uint8_t ep_address);
  zx_status_t UsbDciEpClearStall(uint8_t ep_address);
  zx_status_t UsbDciCancelAll(uint8_t endpoint);
  size_t UsbDciGetRequestSize();

  // USB host controller protocol implementation.
  void UsbHciRequestQueue(usb_request_t* usb_request,
                          const usb_request_complete_callback_t* complete_cb);
  void UsbHciSetBusInterface(const usb_bus_interface_protocol_t* bus_intf);
  size_t UsbHciGetMaxDeviceCount();
  zx_status_t UsbHciEnableEndpoint(uint32_t device_id, const usb_endpoint_descriptor_t* ep_desc,
                                   const usb_ss_ep_comp_descriptor_t* ss_com_desc, bool enable);
  uint64_t UsbHciGetCurrentFrame();
  zx_status_t UsbHciConfigureHub(uint32_t device_id, usb_speed_t speed,
                                 const usb_hub_descriptor_t* desc, bool multi_tt);
  zx_status_t UsbHciHubDeviceAdded(uint32_t device_id, uint32_t port, usb_speed_t speed);
  zx_status_t UsbHciHubDeviceRemoved(uint32_t device_id, uint32_t port);
  zx_status_t UsbHciHubDeviceReset(uint32_t device_id, uint32_t port);
  zx_status_t UsbHciResetEndpoint(uint32_t device_id, uint8_t ep_address);
  zx_status_t UsbHciResetDevice(uint32_t hub_address, uint32_t device_id);
  size_t UsbHciGetMaxTransferSize(uint32_t device_id, uint8_t ep_address);
  zx_status_t UsbHciCancelAll(uint32_t device_id, uint8_t ep_address);
  size_t UsbHciGetRequestSize();

  // FIDL messages
  void Enable(EnableCompleter::Sync& completer) override;
  void Disable(DisableCompleter::Sync& completer) override;
  void Connect(ConnectCompleter::Sync& completer) override;
  void Disconnect(DisconnectCompleter::Sync& completer) override;

  // Public for unit tests.
  void SetConnected(bool connected);

  UsbVirtualEp* ep(uint8_t index) { return &eps_[index]; }
  async_dispatcher_t* device_dispatcher() { return device_dispatcher_.async_dispatcher(); }

 private:
  DISALLOW_COPY_ASSIGN_AND_MOVE(UsbVirtualBus);

  friend class UsbVirtualEp;

  zx_status_t Init();
  zx_status_t CreateDevice();
  zx_status_t CreateHost();
  void ProcessRequests();
  void HandleControl(RequestVariant req);
  zx_status_t SetStall(uint8_t ep_address, bool stall);

  async_dispatcher_t* dispatcher_;

  // Reference to class that implements the virtual device controller protocol.
  std::unique_ptr<UsbVirtualDevice> device_;
  // Reference to class that implements the virtual host controller protocol.
  std::unique_ptr<UsbVirtualHost> host_;

  // Callbacks to the USB peripheral driver.
  ddk::UsbDciInterfaceProtocolClient dci_intf_;
  // Callbacks to the USB bus driver.
  ddk::UsbBusInterfaceProtocolClient bus_intf_;

  UsbVirtualEp eps_[USB_MAX_EPS];

  fdf::SynchronizedDispatcher device_dispatcher_;
  libsync::Completion device_dispatcher_shutdown_wait_;
  async::TaskClosureMethod<UsbVirtualBus, &UsbVirtualBus::ProcessRequests> process_requests_{this};
  // Host-side lock
  fbl::Mutex lock_;

  // Device-side lock
  fbl::Mutex device_lock_ __TA_ACQUIRED_AFTER(lock_);
  fbl::Mutex connection_lock_ __TA_ACQUIRED_BEFORE(device_lock_);
  // True when the virtual bus is connected.
  bool connected_ __TA_GUARDED(connection_lock_) = false;
  // Used to shut down our thread when this driver is unbinding.
  bool unbinding_ __TA_GUARDED(device_lock_) = false;
  std::optional<ddk::UnbindTxn> unbind_txn_;
  // Tracks the number of control requests currently in progress.
  size_t num_pending_control_reqs_ __TA_GUARDED(device_lock_) = 0;
  // Signalled once the device is ready to complete unbinding.
  // This is once all pending control requests have completed,
  // and any newly queued requests would be immediately completed with an error.
  fbl::ConditionVariable complete_unbind_signal_ __TA_GUARDED(device_lock_);
  std::optional<DisconnectCompleter::Async> disconnect_completer_;
  thrd_t disconnect_thread_;
  std::optional<DisableCompleter::Async> disable_completer_;
  thrd_t disable_thread_;

  component::OutgoingDirectory outgoing_;
  fidl::ServerBindingGroup<fuchsia_hardware_usb_hci::UsbHci> hci_bindings_;
  fidl::ServerBindingGroup<fuchsia_hardware_usb_dci::UsbDci> dci_bindings_;
};

}  // namespace usb_virtual_bus

#endif  // SRC_DEVICES_USB_DRIVERS_USB_VIRTUAL_BUS_USB_VIRTUAL_BUS_H_
