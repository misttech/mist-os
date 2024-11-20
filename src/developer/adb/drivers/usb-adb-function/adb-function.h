// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_ADB_DRIVERS_USB_ADB_FUNCTION_ADB_FUNCTION_H_
#define SRC_DEVELOPER_ADB_DRIVERS_USB_ADB_FUNCTION_ADB_FUNCTION_H_

#include <endian.h>
#include <fidl/fuchsia.hardware.adb/cpp/fidl.h>
#include <fidl/fuchsia.hardware.usb.function/cpp/fidl.h>
#include <fuchsia/hardware/usb/function/cpp/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/ddk/driver.h>
#include <lib/sync/cpp/completion.h>
#include <zircon/compiler.h>

#include <queue>

#include <ddktl/device.h>
#include <ddktl/protocol/empty-protocol.h>
#include <fbl/auto_lock.h>
#include <fbl/mutex.h>
#include <usb-endpoint/usb-endpoint-client.h>
#include <usb/descriptors.h>

namespace usb_adb_function {

constexpr uint16_t kBulkMaxPacket = 512;

namespace fadb = fuchsia_hardware_adb;
namespace fendpoint = fuchsia_hardware_usb_endpoint;

class UsbAdbDevice;
using UsbAdb = ddk::Device<UsbAdbDevice, ddk::Suspendable, ddk::Unbindable,
                           ddk::Messageable<fadb::Device>::Mixin>;

// Implements USB ADB function driver.
// Components implementing ADB protocol should open a AdbImpl FIDL connection to dev-class/adb/xxx
// supported by this class to queue ADB messages. ADB protocol component can provide a client
// end channel to AdbInterface during Start method call to receive ADB messages sent by the host.
class UsbAdbDevice : public UsbAdb,
                     public ddk::UsbFunctionInterfaceProtocol<UsbAdbDevice>,
                     public ddk::EmptyProtocol<ZX_PROTOCOL_ADB>,
                     public fidl::Server<fadb::UsbAdbImpl> {
 public:
  // Driver bind method.
  static zx_status_t Bind(void* ctx, zx_device_t* parent);

  explicit UsbAdbDevice(zx_device_t* parent, uint32_t bulk_tx_count, uint32_t bulk_rx_count,
                        uint32_t vmo_data_size)
      : UsbAdb(parent),
        bulk_tx_count_(bulk_tx_count),
        bulk_rx_count_(bulk_rx_count),
        vmo_data_size_(vmo_data_size),
        function_(parent) {
    loop_.StartThread("usb-adb-loop");
    dispatcher_ = loop_.dispatcher();
  }

  // Initialize endpoints and request pools.
  zx_status_t Init();

  // DDK lifecycle methods.
  void DdkRelease();
  void DdkSuspend(ddk::SuspendTxn txn);
  void DdkUnbind(ddk::UnbindTxn txn);

  // UsbFunctionInterface methods.
  size_t UsbFunctionInterfaceGetDescriptorsSize();
  void UsbFunctionInterfaceGetDescriptors(uint8_t* out_descriptors_buffer, size_t descriptors_size,
                                          size_t* out_descriptors_actual);
  zx_status_t UsbFunctionInterfaceControl(const usb_setup_t* setup, const uint8_t* write_buffer,
                                          size_t write_size, uint8_t* out_read_buffer,
                                          size_t read_size, size_t* out_read_actual);
  zx_status_t UsbFunctionInterfaceSetConfigured(bool configured, usb_speed_t speed);
  zx_status_t UsbFunctionInterfaceSetInterface(uint8_t interface, uint8_t alt_setting);

  // fadb::Device methods.
  void Start(StartRequestView request, StartCompleter::Sync& completer) override;
  void Stop(StopCompleter::Sync& completer) override;

  // Helper method called when fadb::Device closes.
  void Stop();

  // fadb::UsbAdbImpl methods.
  void QueueTx(QueueTxRequest& request, QueueTxCompleter::Sync& completer) override;
  void Receive(ReceiveCompleter::Sync& completer) override;

  // Public for testing
  void SetShutdownCallback(fit::callback<void()> cb) {
    fbl::AutoLock _(&lock_);
    shutdown_callback_ = std::move(cb);
  }

 private:
  const uint32_t bulk_tx_count_;
  const uint32_t bulk_rx_count_;
  const size_t vmo_data_size_;

  // Structure to store pending transfer requests when there are not enough USB request buffers.
  struct txn_req_t {
    QueueTxRequest request;
    size_t start = 0;
    QueueTxCompleter::Async completer;
  };

  // Helper method to perform bookkeeping and insert requests back to the free pool.
  zx_status_t InsertUsbRequest(fuchsia_hardware_usb_request::Request req,
                               usb::EndpointClient<UsbAdbDevice>& ep);

  // Helper method to get free request buffer and queue the request for transmitting.
  zx::result<> SendLocked() __TA_REQUIRES(bulk_in_ep_.mutex_);
  // Helper method to get free request buffer and queue the request for receiving.
  void ReceiveLocked() __TA_REQUIRES(bulk_out_ep_.mutex_);

  // USB request completion callback methods.
  void TxComplete(fendpoint::Completion completion);
  void RxComplete(fendpoint::Completion completion);

  // Helper method to configure endpoints
  zx_status_t ConfigureEndpoints(bool enable);

  uint8_t bulk_out_addr() const { return descriptors_.bulk_out_ep.b_endpoint_address; }
  uint8_t bulk_in_addr() const { return descriptors_.bulk_in_ep.b_endpoint_address; }

  bool Online() const {
    fbl::AutoLock _(&lock_);
    return (status_ == fadb::StatusFlags::kOnline) && !shutdown_callback_;
  }

  // Called when shutdown is in progress and all pending requests are completed. Invokes shutdown
  // completion callback.
  void ShutdownComplete() __TA_REQUIRES(lock_);

  ddk::UsbFunctionProtocolClient function_;

  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
  async_dispatcher_t* dispatcher_;

  // UsbAdbImpl service binding. This is created when client calls Start.
  std::optional<fidl::ServerBinding<fadb::UsbAdbImpl>> adb_binding_;

  // Set once the interface is configured.
  fadb::StatusFlags status_ __TA_GUARDED(lock_) = fadb::StatusFlags(0);

  // Holds suspend/unbind DDK callback to be invoked once shutdown is complete.
  fit::callback<void()> shutdown_callback_ __TA_GUARDED(lock_);
  // `stop_completed_` ensures that `shutdown_callback_` is only called after `Stop()` has finished
  // all its necessary operations including deconfiguring endpoints, etc. In practice, this is not
  // important, but this facilitates orderly shutdown which avoids flakes in tests.
  std::atomic_bool stop_completed_ __TA_GUARDED(lock_) = false;

  // This driver uses 4 locks to avoid race conditions in different sub-parts of the driver. The
  // OUT/IN endpoints each contain one mutex, where bulk_in_ep_.mutex_ is used to avoid race
  // conditions w.r.t transmit buffers. bulk_out_ep_.mutex_ is used to avoid race conditions w.r.t
  // receive buffers. lock_ is used for all driver internal states. Alternatively a single lock
  // (lock_) could have been used for TX, RX and driver states, but that will serialize TX methods
  // w.r.t RX. Hence the separation of locks.
  //
  // NOTE: In order to maintain reentrancy, do not hold any lock when invoking callbacks/methods
  // that can reenter the driver methods.
  //
  // As for lock ordering, IN/OUT mutex_s must be the highest order lock i.e. it must be
  // acquired before lock_ when both locks are held. IN/OUT mutex_s are
  // never acquired together.

  // Lock for guarding driver states. This should be held for only a short duration and is the inner
  // most lock in all cases.
  mutable fbl::Mutex lock_ __TA_ACQUIRED_AFTER(bulk_in_ep_.mutex_)
      __TA_ACQUIRED_AFTER(bulk_out_ep_.mutex_);

  // USB ADB interface descriptor.
  struct {
    usb_interface_descriptor_t adb_intf;
    usb_endpoint_descriptor_t bulk_out_ep;
    usb_endpoint_descriptor_t bulk_in_ep;
  } descriptors_ = {
      .adb_intf =
          {
              .b_length = sizeof(usb_interface_descriptor_t),
              .b_descriptor_type = USB_DT_INTERFACE,
              .b_interface_number = 0,  // set later during AllocInterface
              .b_alternate_setting = 0,
              .b_num_endpoints = 2,
              .b_interface_class = USB_CLASS_VENDOR,
              .b_interface_sub_class = USB_SUBCLASS_ADB,
              .b_interface_protocol = USB_PROTOCOL_ADB,
              .i_interface = 0,  // This is set in adb
          },
      .bulk_out_ep =
          {
              .b_length = sizeof(usb_endpoint_descriptor_t),
              .b_descriptor_type = USB_DT_ENDPOINT,
              .b_endpoint_address = 0,  // set later during AllocEp
              .bm_attributes = USB_ENDPOINT_BULK,
              .w_max_packet_size = htole16(kBulkMaxPacket),
              .b_interval = 0,
          },
      .bulk_in_ep =
          {
              .b_length = sizeof(usb_endpoint_descriptor_t),
              .b_descriptor_type = USB_DT_ENDPOINT,
              .b_endpoint_address = 0,  // set later during AllocEp
              .bm_attributes = USB_ENDPOINT_BULK,
              .w_max_packet_size = htole16(kBulkMaxPacket),
              .b_interval = 0,
          },
  };

  zx_status_t InitEndpoint(fidl::ClientEnd<fuchsia_hardware_usb_function::UsbFunction>& client,
                           uint8_t direction, uint8_t* ep_addrs,
                           usb::EndpointClient<UsbAdbDevice>& ep, uint32_t req_count);

  // Bulk OUT/RX endpoint
  usb::EndpointClient<UsbAdbDevice> bulk_out_ep_{usb::EndpointType::BULK, this,
                                                 std::mem_fn(&UsbAdbDevice::RxComplete)};
  // Queue of pending Receive requests from client.
  std::queue<ReceiveCompleter::Async> rx_requests_ __TA_GUARDED(bulk_out_ep_.mutex_);
  // pending_replies_ only used for bulk_out_ep_
  std::queue<fendpoint::Completion> pending_replies_ __TA_GUARDED(bulk_out_ep_.mutex_);

  // Bulk IN/TX endpoint
  usb::EndpointClient<UsbAdbDevice> bulk_in_ep_{usb::EndpointType::BULK, this,
                                                std::mem_fn(&UsbAdbDevice::TxComplete)};
  // Queue of pending transfer requests that need to be transmitted once the BULK IN request buffers
  // become available.
  std::queue<txn_req_t> tx_pending_reqs_ __TA_GUARDED(bulk_in_ep_.mutex_);
};

}  // namespace usb_adb_function

#endif  // SRC_DEVELOPER_ADB_DRIVERS_USB_ADB_FUNCTION_ADB_FUNCTION_H_
