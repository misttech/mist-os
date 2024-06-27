// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_INPUT_DRIVERS_HID_HID_H_
#define SRC_UI_INPUT_DRIVERS_HID_HID_H_

#include <fidl/fuchsia.hardware.hidbus/cpp/fidl.h>
#include <fidl/fuchsia.hardware.hidbus/cpp/wire.h>
#include <fidl/fuchsia.hardware.input/cpp/wire.h>
#include <fuchsia/hardware/hiddevice/cpp/banjo.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/hid-parser/item.h>
#include <lib/hid-parser/parser.h>
#include <lib/hid-parser/usages.h>

#include <array>
#include <memory>
#include <set>
#include <vector>

#include <ddktl/device.h>
#include <ddktl/fidl.h>
#include <ddktl/protocol/empty-protocol.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/mutex.h>
#include <fbl/ref_ptr.h>

#include "hid-instance.h"

namespace hid_driver {

using input_report_id_t = uint8_t;
struct HidPageUsage {
  uint16_t page;
  uint32_t usage;

  friend bool operator<(const HidPageUsage& l, const HidPageUsage& r) {
    return std::tie(l.page, l.usage) < std::tie(r.page, r.usage);
  }
};

class HidDevice;

using HidDeviceType =
    ddk::Device<HidDevice, ddk::Messageable<fuchsia_hardware_input::Controller>::Mixin,
                ddk::Unbindable>;

class HidDevice : public HidDeviceType,
                  public ddk::HidDeviceProtocol<HidDevice, ddk::base_protocol>,
                  public fidl::WireAsyncEventHandler<fuchsia_hardware_hidbus::Hidbus> {
 public:
  explicit HidDevice(zx_device_t* parent, fidl::ClientEnd<fuchsia_hardware_hidbus::Hidbus> hidbus)
      : HidDeviceType(parent),
        outgoing_(fdf::Dispatcher::GetCurrent()->async_dispatcher()),
        hidbus_(std::move(hidbus), fdf::Dispatcher::GetCurrent()->async_dispatcher(), this) {}
  ~HidDevice() override = default;

  zx_status_t Bind();
  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease();

  void OpenSession(OpenSessionRequestView request, OpenSessionCompleter::Sync& completer) override;

  // |HidDeviceProtocol|
  zx_status_t HidDeviceRegisterListener(const hid_report_listener_protocol_t* listener);
  // |HidDeviceProtocol|
  void HidDeviceUnregisterListener();
  // |HidDeviceProtocol|
  zx_status_t HidDeviceGetDescriptor(uint8_t* out_descriptor_data, size_t descriptor_count,
                                     size_t* out_descriptor_actual);
  // |HidDeviceProtocol|
  zx_status_t HidDeviceGetReport(hid_report_type_t rpt_type, uint8_t rpt_id,
                                 uint8_t* out_report_data, size_t report_count,
                                 size_t* out_report_actual);
  // |HidDeviceProtocol|
  zx_status_t HidDeviceSetReport(hid_report_type_t rpt_type, uint8_t rpt_id,
                                 const uint8_t* report_data, size_t report_count);
  // |HidDeviceProtocol|
  void HidDeviceGetHidDeviceInfo(hid_device_info_t* out_info);

  // fidl::WireAsyncEventHandler<fuchsia_hardware_hidbus::Hidbus> Methods.
  void OnReportReceived(
      fidl::WireEvent<fuchsia_hardware_hidbus::Hidbus::OnReportReceived>* event) override;

  size_t GetMaxInputReportSize();

  size_t GetReportSizeById(input_report_id_t id, fuchsia_hardware_hidbus::wire::ReportType type);
  // Owned by HidDevice. Will be destructed when HidDevice is destructed.
  const fuchsia_hardware_hidbus::HidInfo& GetHidInfo() { return info_; }

  fidl::WireSharedClient<fuchsia_hardware_hidbus::Hidbus>& GetHidbusProtocol() { return hidbus_; }

  zx::result<fbl::RefPtr<HidInstance>> CreateInstance(
      async_dispatcher_t* dispatcher, fidl::ServerEnd<fuchsia_hardware_input::Device> session);

  size_t GetReportDescLen() { return hid_report_desc_.size(); }
  const uint8_t* GetReportDesc() { return hid_report_desc_.data(); }

  const char* GetName();

  void RemoveInstance(HidInstance& instance);

 private:
  zx_status_t ProcessReportDescriptor();
  zx_status_t InitReassemblyBuffer();
  void ReleaseReassemblyBuffer();
  zx_status_t SetReportDescriptor();

  void ParseUsagePage(const hid::ReportDescriptor* descriptor);

  component::OutgoingDirectory outgoing_;
  fidl::ServerBindingGroup<fuchsia_hardware_input::Controller> bindings_;

  std::set<HidPageUsage> page_usage_;
  fuchsia_hardware_hidbus::HidInfo info_;
  // TODO (rdzhuang): Use fidl::WireSharedClient to avoid thread synchronization errors when calling
  // from Banjo methods. Change to fidl::WireClient when we've ripped out Banjo.
  fidl::WireSharedClient<fuchsia_hardware_hidbus::Hidbus> hidbus_;

  // Reassembly buffer for input events too large to fit in a single interrupt
  // transaction.
  uint8_t* rbuf_ = nullptr;
  size_t rbuf_size_ = 0;
  size_t rbuf_filled_ = 0;
  size_t rbuf_needed_ = 0;

  std::vector<uint8_t> hid_report_desc_;

  hid::DeviceDescriptor* parsed_hid_desc_ = nullptr;
  size_t num_reports_ = 0;

  fbl::Mutex instance_lock_;
  fbl::DoublyLinkedList<fbl::RefPtr<HidInstance>> instance_list_ __TA_GUARDED(instance_lock_);

  std::array<char, ZX_DEVICE_NAME_MAX + 1> name_;

  fbl::Mutex listener_lock_;
  ddk::HidReportListenerProtocolClient report_listener_ __TA_GUARDED(listener_lock_);
};

}  // namespace hid_driver

#endif  // SRC_UI_INPUT_DRIVERS_HID_HID_H_
