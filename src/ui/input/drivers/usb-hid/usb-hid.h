// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_INPUT_DRIVERS_USB_HID_USB_HID_H_
#define SRC_UI_INPUT_DRIVERS_USB_HID_USB_HID_H_

#include <fidl/fuchsia.hardware.hidbus/cpp/wire.h>
#include <fuchsia/hardware/usb/cpp/banjo.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/sync/completion.h>

#include <memory>
#include <thread>

#include <ddktl/device.h>
#include <ddktl/protocol/empty-protocol.h>
#include <fbl/condition_variable.h>
#include <fbl/mutex.h>
#include <usb/hid.h>
#include <usb/usb.h>

namespace usb_hid {

class UsbHidbus;
using DeviceType = ddk::Device<UsbHidbus, ddk::Unbindable>;

class UsbHidbus : public DeviceType,
                  public ddk::EmptyProtocol<ZX_PROTOCOL_HIDBUS>,
                  public fidl::WireServer<fuchsia_hardware_hidbus::Hidbus> {
 public:
  explicit UsbHidbus(zx_device_t* device)
      : DeviceType(device), outgoing_(fdf::Dispatcher::GetCurrent()->async_dispatcher()) {}

  // Methods required by the ddk mixins.
  void UsbInterruptCallback(usb_request_t* req);
  // fuchsia_hardware_hidbus methods.
  void Query(QueryCompleter::Sync& completer) override;
  void Start(StartCompleter::Sync& completer) override;
  void Stop(StopCompleter::Sync& completer) override;
  void GetDescriptor(fuchsia_hardware_hidbus::wire::HidbusGetDescriptorRequest* request,
                     GetDescriptorCompleter::Sync& completer) override;
  void SetDescriptor(fuchsia_hardware_hidbus::wire::HidbusSetDescriptorRequest* request,
                     SetDescriptorCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void GetReport(fuchsia_hardware_hidbus::wire::HidbusGetReportRequest* request,
                 GetReportCompleter::Sync& completer) override;
  void SetReport(fuchsia_hardware_hidbus::wire::HidbusSetReportRequest* request,
                 SetReportCompleter::Sync& completer) override;
  void GetIdle(fuchsia_hardware_hidbus::wire::HidbusGetIdleRequest* request,
               GetIdleCompleter::Sync& completer) override;
  void SetIdle(fuchsia_hardware_hidbus::wire::HidbusSetIdleRequest* request,
               SetIdleCompleter::Sync& completer) override;
  void GetProtocol(GetProtocolCompleter::Sync& completer) override;
  void SetProtocol(fuchsia_hardware_hidbus::wire::HidbusSetProtocolRequest* request,
                   SetProtocolCompleter::Sync& completer) override;
  zx_status_t UsbHidControl(uint8_t req_type, uint8_t request, uint16_t value, uint16_t index,
                            void* data, size_t length, size_t* out_length);
  zx_status_t UsbHidControlIn(uint8_t req_type, uint8_t request, uint16_t value, uint16_t index,
                              void* data, size_t length, size_t* out_length);
  zx_status_t UsbHidControlOut(uint8_t req_type, uint8_t request, uint16_t value, uint16_t index,
                               const void* data, size_t length, size_t* out_length);

  void DdkUnbind(ddk::UnbindTxn txn);
  void UsbHidRelease();
  void DdkRelease();
  void FindDescriptors(usb::Interface interface, usb_hid_descriptor_t** hid_desc,
                       const usb_endpoint_descriptor_t** endptin,
                       const usb_endpoint_descriptor_t** endptout);
  zx_status_t Bind(ddk::UsbProtocolClient usbhid);

 private:
  component::OutgoingDirectory outgoing_;
  fidl::ServerBindingGroup<fuchsia_hardware_hidbus::Hidbus> binding_;
  std::atomic_uint32_t start_ = 0;

  std::optional<usb::InterfaceList> usb_interface_list_;

  // These pointers are valid as long as usb_interface_list_ is valid.
  usb_hid_descriptor_t* hid_desc_ = nullptr;

  uint8_t endptin_address_ = 0;
  uint8_t endptout_address_ = 0;
  // This boolean is set to true for a usb device that has an interrupt out endpoint. The interrupt
  // out endpoint is used to send reports to the device. (the SET report protocol).
  bool has_endptout_ = false;
  size_t endptout_max_size_ = 0;

  fidl::Arena<> arena_;
  fuchsia_hardware_hidbus::wire::HidInfo info_;
  usb_request_t* req_ = nullptr;
  usb_request_t* request_out_ = nullptr;
  bool req_queued_ = false;

  ddk::UsbProtocolClient usb_ = {};

  uint8_t interface_ = 0;
  usb_desc_iter_t desc_iter_ = {};
  size_t parent_req_size_ = 0;

  std::thread unbind_thread_;
  sync_completion_t set_report_complete_;
};

}  // namespace usb_hid

#endif  // SRC_UI_INPUT_DRIVERS_USB_HID_USB_HID_H_
