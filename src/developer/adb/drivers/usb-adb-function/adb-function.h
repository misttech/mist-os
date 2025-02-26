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
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/sync/cpp/completion.h>
#include <zircon/compiler.h>

#include <mutex>
#include <queue>

#include <usb-endpoint/usb-endpoint-client.h>
#include <usb/descriptors.h>

namespace usb_adb_function {

constexpr uint32_t kBulkTxCount = 16;
constexpr uint32_t kBulkRxCount = 16;
constexpr size_t kVmoDataSize = 2048;

constexpr uint16_t kBulkMaxPacket = 512;

constexpr char kDeviceName[] = "usb-adb-function";

namespace fadb = fuchsia_hardware_adb;
namespace fendpoint = fuchsia_hardware_usb_endpoint;

// Implements USB ADB function driver.
// Components implementing ADB protocol should open a AdbImpl FIDL connection to dev-class/adb/xxx
// supported by this class to queue ADB messages. ADB protocol component can provide a client
// end channel to AdbInterface during StartAdb method call to receive ADB messages sent by the
// host.
class UsbAdbDevice : public fdf::DriverBase,
                     public ddk::UsbFunctionInterfaceProtocol<UsbAdbDevice>,
                     public fidl::WireServer<fadb::Device>,
                     public fidl::Server<fadb::UsbAdbImpl> {
 public:
  explicit UsbAdbDevice(fdf::DriverStartArgs start_args,
                        fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : fdf::DriverBase("usb_adb", std::move(start_args), std::move(driver_dispatcher)),
        checker_(dispatcher()),
        devfs_connector_(fit::bind_member<&UsbAdbDevice::Serve>(this)) {
    // TODO(https://fxrev.dev/333883656): Use SynchronizedDispatcher with
    // FDF_DISPATCHER_OPTION_ALLOW_SYNC_CALLS instead.
    usb_loop_.StartThread("usb-adb-loop");
    usb_dispatcher_ = usb_loop_.dispatcher();
  }

  // Driver lifecycle methods.
  zx::result<> Start() override;
  void PrepareStop(fdf::PrepareStopCompleter completer) override;

  // UsbFunctionInterface methods.
  size_t UsbFunctionInterfaceGetDescriptorsSize();
  void UsbFunctionInterfaceGetDescriptors(uint8_t* buffer, size_t buffer_size, size_t* out_actual);
  zx_status_t UsbFunctionInterfaceControl(const usb_setup_t* setup, const uint8_t* write_buffer,
                                          size_t write_size, uint8_t* out_read_buffer,
                                          size_t read_size, size_t* out_read_actual);
  zx_status_t UsbFunctionInterfaceSetConfigured(bool configured, usb_speed_t speed);
  zx_status_t UsbFunctionInterfaceSetInterface(uint8_t interface, uint8_t alt_setting);

  // fadb::Device methods.
  void StartAdb(StartAdbRequestView request, StartAdbCompleter::Sync& completer) override;
  void StopAdb(StopAdbCompleter::Sync& completer) override;

  // fadb::UsbAdbImpl methods.
  void QueueTx(QueueTxRequest& request, QueueTxCompleter::Sync& completer) override;
  void Receive(ReceiveCompleter::Sync& completer) override;

  // Public for testing
  void SetShutdownCallback(fit::callback<void()> cb) {
    std::lock_guard<async::sequence_checker> _(checker_);
    shutdown_callback_ = std::move(cb);
  }

 private:
  mutable async::sequence_checker checker_;

  // Helper methods.
  void StopImpl() __TA_REQUIRES(checker_);

  // Helpers and fields for exposing the service via devfs.
  void Serve(fidl::ServerEnd<fadb::Device> server) {
    device_bindings_.AddBinding(dispatcher(), std::move(server), this, fidl::kIgnoreBindingClosure);
  }
  zx::result<> CreateDevfsNode();

  fidl::ServerBindingGroup<fadb::Device> device_bindings_;
  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;
  driver_devfs::Connector<fadb::Device> devfs_connector_;

  // Structure to store pending transfer requests when there are not enough USB request buffers.
  struct txn_req_t {
    QueueTxRequest request;
    size_t start = 0;
    QueueTxCompleter::Async completer;
  };

  // Helper method to perform bookkeeping and insert requests back to the free pool.
  zx_status_t InsertUsbRequest(fuchsia_hardware_usb_request::Request req,
                               usb::EndpointClient<UsbAdbDevice>& ep) __TA_REQUIRES(checker_);

  // Helper method to get free request buffer and queue the request for transmitting.
  zx::result<> SendQueued() __TA_REQUIRES(checker_);
  // Helper method to get free request buffer and queue the request for receiving.
  void ReceiveQueued() __TA_REQUIRES(checker_);

  // USB request completion callback methods - these run on the driver
  // dispatcher.
  void TxComplete(fendpoint::Completion completion) __TA_REQUIRES(checker_);
  void RxComplete(fendpoint::Completion completion) __TA_REQUIRES(checker_);

  // USB request completion callback methods - these can run on any thread.
  void TxCompleteCallback(fendpoint::Completion completion);
  void RxCompleteCallback(fendpoint::Completion completion);

  // Helper method to configure endpoints
  zx_status_t ConfigureEndpoints(bool enable) __TA_REQUIRES(checker_);

  uint8_t bulk_out_addr() const { return descriptors_.bulk_out_ep.b_endpoint_address; }
  uint8_t bulk_in_addr() const { return descriptors_.bulk_in_ep.b_endpoint_address; }

  bool Online() const __TA_REQUIRES(checker_) {
    return (status_ == fadb::StatusFlags::kOnline) && !shutdown_callback_.has_value();
  }

  // Called when shutdown is in progress and all pending requests are completed. Invokes shutdown
  // completion callback.
  void ShutdownComplete() __TA_REQUIRES(checker_);

  ddk::UsbFunctionProtocolClient function_;

  // Loop and dispatcher used in the background by the USB endpoint clients.
  async::Loop usb_loop_{&kAsyncLoopConfigNeverAttachToThread};
  async_dispatcher_t* usb_dispatcher_;

  // UsbAdbImpl service binding. This is created when client calls StartAdb.
  std::optional<fidl::ServerBinding<fadb::UsbAdbImpl>> adb_binding_ __TA_GUARDED(checker_);

  // Set once the interface is configured.
  fadb::StatusFlags status_ __TA_GUARDED(checker_) = fadb::StatusFlags(0);

  // Holds Stop callback to be invoked once shutdown is complete.
  std::optional<fit::callback<void()>> shutdown_callback_ __TA_GUARDED(checker_);
  // `stop_completed_` ensures that `shutdown_callback_` is only called after `Stop()` has finished
  // all its necessary operations including deconfiguring endpoints, etc. In practice, this is not
  // important, but this facilitates orderly shutdown which avoids flakes in tests.
  bool stop_completed_ __TA_GUARDED(checker_) = false;

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
                           usb::EndpointClient<UsbAdbDevice>& ep, uint32_t req_count)
      __TA_REQUIRES(checker_);

  // Bulk OUT/RX endpoint
  usb::EndpointClient<UsbAdbDevice> bulk_out_ep_{usb::EndpointType::BULK, this,
                                                 std::mem_fn(&UsbAdbDevice::RxCompleteCallback)};
  // Queue of pending Receive requests from client.
  std::queue<ReceiveCompleter::Async> rx_requests_ __TA_GUARDED(checker_);
  // pending_replies_ only used for bulk_out_ep_
  std::queue<fendpoint::Completion> pending_replies_ __TA_GUARDED(checker_);

  // Bulk IN/TX endpoint
  usb::EndpointClient<UsbAdbDevice> bulk_in_ep_{usb::EndpointType::BULK, this,
                                                std::mem_fn(&UsbAdbDevice::TxCompleteCallback)};
  // Queue of pending transfer requests that need to be transmitted once the BULK IN request buffers
  // become available.
  std::queue<txn_req_t> tx_pending_reqs_ __TA_GUARDED(checker_);
};

}  // namespace usb_adb_function

#endif  // SRC_DEVELOPER_ADB_DRIVERS_USB_ADB_FUNCTION_ADB_FUNCTION_H_
