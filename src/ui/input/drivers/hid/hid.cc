// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "hid.h"

#include <assert.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/trace/event.h>
#include <lib/hid/boot.h>
#include <stdlib.h>
#include <string.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/syscalls.h>

#include <algorithm>
#include <map>
#include <memory>

#include <bind/fuchsia/hid/cpp/bind.h>
#include <fbl/auto_lock.h>

#include "src/ui/input/drivers/hid/hid-instance.h"

namespace hid_driver {

namespace fhidbus = fuchsia_hardware_hidbus;

namespace {

#define MAKE_KEY(PAGE, USAGE)                                  \
  HidPageUsage {                                               \
    .page = static_cast<uint16_t>(hid::usage::Page::k##PAGE),  \
    .usage = static_cast<uint32_t>(hid::usage::PAGE::k##USAGE) \
  }

std::map<HidPageUsage, const std::string> kBindPropKeyMap({
    {MAKE_KEY(Consumer, ConsumerControl), bind_fuchsia_hid::CONSUMER__CONSUMER_CONTROL},
    {MAKE_KEY(Digitizer, TouchPad), bind_fuchsia_hid::DIGITIZER__TOUCH_PAD},
    {MAKE_KEY(Digitizer, TouchScreen), bind_fuchsia_hid::DIGITIZER__TOUCH_SCREEN},
    {MAKE_KEY(Digitizer, TouchScreenConfiguration),
     bind_fuchsia_hid::DIGITIZER__TOUCH_SCREEN_CONFIGURATION},
    {MAKE_KEY(FidoAlliance, Undefined), bind_fuchsia_hid::FIDO_ALLIANCE},  // only match page
    {MAKE_KEY(GenericDesktop, Keyboard), bind_fuchsia_hid::GENERIC_DESKTOP__KEYBOARD},
    {MAKE_KEY(GenericDesktop, Mouse), bind_fuchsia_hid::GENERIC_DESKTOP__MOUSE},
    {MAKE_KEY(Sensor, Undefined), bind_fuchsia_hid::SENSOR},  // only match page
});

zx::result<HidPageUsage> FindProp(HidPageUsage key) {
  if (kBindPropKeyMap.find(key) != kBindPropKeyMap.end()) {
    return zx::ok(key);
  }

  // Only match page for kBindPropKeyMap entries where usage is Undefined
  for (auto const& [page_usage, value] : kBindPropKeyMap) {
    if (page_usage.usage) {
      continue;
    }

    if (page_usage.page == key.page) {  // pages match
      return zx::ok(page_usage);
    }
  }
  return zx::error(ZX_ERR_NOT_FOUND);
}

}  // namespace

void HidDevice::ParseUsagePage(const hid::ReportDescriptor* descriptor) {
  auto collection = hid::GetAppCollection(&descriptor->input_fields[0]);
  if (collection == nullptr) {
    return;
  }

  page_usage_.insert(
      HidPageUsage{.page = collection->usage.page, .usage = collection->usage.usage});
}

size_t HidDevice::GetReportSizeById(input_report_id_t id, fuchsia_hardware_input::ReportType type) {
  for (size_t i = 0; i < parsed_hid_desc_->rep_count; i++) {
    // If we have more than one report, get the report with the right id. If we only have
    // one report, then always match that report.
    if ((parsed_hid_desc_->report[i].report_id == id) || (parsed_hid_desc_->rep_count == 1)) {
      switch (type) {
        case fuchsia_hardware_input::ReportType::kInput:
          return parsed_hid_desc_->report[i].input_byte_sz;
        case fuchsia_hardware_input::ReportType::kOutput:
          return parsed_hid_desc_->report[i].output_byte_sz;
        case fuchsia_hardware_input::ReportType::kFeature:
          return parsed_hid_desc_->report[i].feature_byte_sz;
      }
    }
  }

  return 0;
}

fuchsia_hardware_input::BootProtocol HidDevice::GetBootProtocol() {
  return static_cast<fuchsia_hardware_input::BootProtocol>(
      info_.boot_protocol().value_or(fhidbus::HidBootProtocol::kNone));
}

zx::result<fbl::RefPtr<HidInstance>> HidDevice::CreateInstance(
    async_dispatcher_t* dispatcher, fidl::ServerEnd<fuchsia_hardware_input::Device> session) {
  zx::event fifo_event;
  if (zx_status_t status = zx::event::create(0, &fifo_event); status != ZX_OK) {
    return zx::error(status);
  }

  fbl::AllocChecker ac;
  fbl::RefPtr instance = fbl::MakeRefCountedChecked<HidInstance>(&ac, this, std::move(fifo_event),
                                                                 dispatcher, std::move(session));
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  fbl::AutoLock lock(&instance_lock_);
  instance_list_.push_back(instance);
  return zx::ok(instance);
}

size_t HidDevice::GetMaxInputReportSize() {
  size_t size = 0;
  for (size_t i = 0; i < parsed_hid_desc_->rep_count; i++) {
    if (parsed_hid_desc_->report[i].input_byte_sz > size)
      size = parsed_hid_desc_->report[i].input_byte_sz;
  }
  return size;
}

zx_status_t HidDevice::ProcessReportDescriptor() {
  hid::ParseResult res = hid::ParseReportDescriptor(hid_report_desc_.data(),
                                                    hid_report_desc_.size(), &parsed_hid_desc_);
  if (res != hid::ParseResult::kParseOk) {
    return ZX_ERR_INTERNAL;
  }

  size_t num_reports = 0;
  for (size_t i = 0; i < parsed_hid_desc_->rep_count; i++) {
    const hid::ReportDescriptor* desc = &parsed_hid_desc_->report[i];
    if (desc->input_count != 0) {
      num_reports++;

      ParseUsagePage(desc);
    }
    if (desc->output_count != 0) {
      num_reports++;
    }
    if (desc->feature_count != 0) {
      num_reports++;
    }
  }
  num_reports_ = num_reports;
  return ZX_OK;
}

void HidDevice::ReleaseReassemblyBuffer() {
  if (rbuf_ != nullptr) {
    free(rbuf_);
  }

  rbuf_ = nullptr;
  rbuf_size_ = 0;
  rbuf_filled_ = 0;
  rbuf_needed_ = 0;
}

zx_status_t HidDevice::InitReassemblyBuffer() {
  ZX_DEBUG_ASSERT(rbuf_ == nullptr);
  ZX_DEBUG_ASSERT(rbuf_size_ == 0);
  ZX_DEBUG_ASSERT(rbuf_filled_ == 0);
  ZX_DEBUG_ASSERT(rbuf_needed_ == 0);

  // TODO(johngro) : Take into account the underlying transport's ability to
  // deliver payloads.  For example, if this is a USB HID device operating at
  // full speed, we can expect it to deliver up to 64 bytes at a time.  If the
  // maximum HID input report size is only 60 bytes, we should not need a
  // reassembly buffer.
  size_t max_report_size = GetMaxInputReportSize();
  rbuf_ = static_cast<uint8_t*>(malloc(max_report_size));
  if (rbuf_ == nullptr) {
    return ZX_ERR_NO_MEMORY;
  }

  rbuf_size_ = max_report_size;
  return ZX_OK;
}

void HidDevice::DdkRelease() {
  ReleaseReassemblyBuffer();
  if (parsed_hid_desc_) {
    FreeDeviceDescriptor(parsed_hid_desc_);
  }
  delete this;
}

void HidDevice::OpenSession(OpenSessionRequestView request, OpenSessionCompleter::Sync& completer) {
  zx::result instance = CreateInstance(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                       std::move(request->session));
  if (instance.is_error()) {
    request->session.Close(instance.error_value());
    return;
  }
}

void HidDevice::DdkUnbind(ddk::UnbindTxn txn) {
  {
    fbl::AutoLock lock(&instance_lock_);
    instance_list_.clear();
  }
  txn.Reply();
}

void HidDevice::OnReportReceived(fidl::WireEvent<fhidbus::Hidbus::OnReportReceived>* event) {
  size_t len = event->buf.count();
  const uint8_t* buf = event->buf.data();
  fbl::AutoLock lock(&instance_lock_);

  while (len) {
    // Start by figuring out if this payload either completes a partially
    // assembled input report or represents an entire input buffer report on
    // its own.
    const uint8_t* rbuf;
    size_t rlen;
    size_t consumed;

    if (rbuf_needed_) {
      // Reassembly is in progress, just continue the process.
      consumed = std::min(len, rbuf_needed_);
      ZX_DEBUG_ASSERT(rbuf_size_ >= rbuf_filled_);
      ZX_DEBUG_ASSERT((rbuf_size_ - rbuf_filled_) >= consumed);

      memcpy(rbuf_ + rbuf_filled_, buf, consumed);

      if (consumed == rbuf_needed_) {
        // reassembly finished.  Reset the bookkeeping and deliver the
        // payload.
        rbuf = rbuf_;
        rlen = rbuf_filled_ + consumed;
        rbuf_filled_ = 0;
        rbuf_needed_ = 0;
      } else {
        // We have not finished the process yet.  Update the bookkeeping
        // and get out.
        rbuf_filled_ += consumed;
        rbuf_needed_ -= consumed;
        break;
      }
    } else {
      // No reassembly is in progress.  Start by identifying this report's
      // size.
      size_t report_size = GetReportSizeById(buf[0], fuchsia_hardware_input::ReportType::kInput);

      // If we don't recognize this report ID, we are in trouble.  Drop
      // the rest of this payload and hope that the next one gets us back
      // on track.
      if (!report_size) {
        zxlogf(DEBUG, "%s: failed to find input report size (report id %u)", name_.data(), buf[0]);
        break;
      }

      // Is the entire report present in this payload?  If so, just go
      // ahead an deliver it directly from the input buffer.
      if (len >= report_size) {
        rbuf = buf;
        consumed = rlen = report_size;
      } else {
        // Looks likes our report is fragmented over multiple buffers.
        // Start the process of reassembly and get out.
        ZX_DEBUG_ASSERT(rbuf_ != nullptr);
        ZX_DEBUG_ASSERT(rbuf_size_ >= report_size);
        memcpy(rbuf_, buf, len);
        rbuf_filled_ = len;
        rbuf_needed_ = report_size - len;
        break;
      }
    }

    ZX_DEBUG_ASSERT(rbuf != nullptr);
    ZX_DEBUG_ASSERT(consumed <= len);
    buf += consumed;
    len -= consumed;

    for (auto& instance : instance_list_) {
      instance.WriteToFifo(rbuf, rlen, event->timestamp);
    }

    {
      fbl::AutoLock lock(&listener_lock_);
      if (report_listener_.is_valid()) {
        report_listener_.ReceiveReport(rbuf, rlen, event->timestamp);
      }
    }
  }
}

zx_status_t HidDevice::HidDeviceRegisterListener(const hid_report_listener_protocol_t* listener) {
  fbl::AutoLock lock(&listener_lock_);

  if (report_listener_.is_valid()) {
    return ZX_ERR_ALREADY_BOUND;
  }
  report_listener_ = ddk::HidReportListenerProtocolClient(listener);

  return ZX_OK;
}

void HidDevice::HidDeviceUnregisterListener() {
  fbl::AutoLock lock(&listener_lock_);
  report_listener_.clear();
}

void HidDevice::HidDeviceGetHidDeviceInfo(hid_device_info_t* out_info) {
  out_info->vendor_id = info_.vendor_id().value_or(0);
  out_info->product_id = info_.product_id().value_or(0);
  out_info->version = info_.version().value_or(0);
  out_info->polling_rate = info_.polling_rate().value_or(0);
}

zx_status_t HidDevice::HidDeviceGetDescriptor(uint8_t* out_descriptor_data, size_t descriptor_count,
                                              size_t* out_descriptor_actual) {
  if (descriptor_count < hid_report_desc_.size()) {
    return ZX_ERR_BUFFER_TOO_SMALL;
  }
  memcpy(out_descriptor_data, hid_report_desc_.data(), hid_report_desc_.size());
  *out_descriptor_actual = hid_report_desc_.size();
  return ZX_OK;
}

zx_status_t HidDevice::HidDeviceGetReport(hid_report_type_t rpt_type, uint8_t rpt_id,
                                          uint8_t* out_report_data, size_t report_count,
                                          size_t* out_report_actual) {
  size_t needed =
      GetReportSizeById(rpt_id, static_cast<fuchsia_hardware_input::ReportType>(rpt_type));
  if (needed == 0) {
    return ZX_ERR_NOT_FOUND;
  }
  if (needed > report_count) {
    return ZX_ERR_BUFFER_TOO_SMALL;
  }
  if (needed > HID_MAX_REPORT_LEN) {
    zxlogf(ERROR, "hid: GetReport: Report size 0x%lx larger than max size 0x%x", needed,
           HID_MAX_REPORT_LEN);
    return ZX_ERR_INTERNAL;
  }

  auto result = hidbus_.sync()->GetReport(static_cast<fuchsia_hardware_input::ReportType>(rpt_type),
                                          rpt_id, needed);
  if (!result.ok()) {
    zxlogf(ERROR, "FIDL transport failed on GetReport(): %s",
           result.error().FormatDescription().c_str());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "HID device failed to get report: %d", result->error_value());
    return result->error_value();
  }
  size_t actual = result.value()->data.count();
  if (actual > report_count) {
    zxlogf(ERROR, "GetReport returned more than expected %zu > %zu. Capping at %zu", actual,
           report_count, report_count);
    actual = report_count;
  }
  memcpy(out_report_data, result.value()->data.data(), actual);
  *out_report_actual = actual;

  return ZX_OK;
}

zx_status_t HidDevice::HidDeviceSetReport(hid_report_type_t rpt_type, uint8_t rpt_id,
                                          const uint8_t* report_data, size_t report_count) {
  size_t needed =
      GetReportSizeById(rpt_id, static_cast<fuchsia_hardware_input::ReportType>(rpt_type));
  if (needed < report_count) {
    return ZX_ERR_BUFFER_TOO_SMALL;
  }

  auto result = hidbus_.sync()->SetReport(
      static_cast<fuchsia_hardware_input::ReportType>(rpt_type), rpt_id,
      fidl::VectorView<uint8_t>::FromExternal(const_cast<uint8_t*>(report_data), report_count));
  if (!result.ok()) {
    zxlogf(ERROR, "FIDL transport failed on SetReport(): %s",
           result.error().FormatDescription().c_str());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "HID device failed to set report: %d", result->error_value());
    return result->error_value();
  }

  return ZX_OK;
}

zx_status_t HidDevice::SetReportDescriptor() {
  {
    auto result = hidbus_.sync()->GetDescriptor(fhidbus::wire::HidDescriptorType::kReport);
    if (!result.ok()) {
      zxlogf(ERROR, "FIDL transport failed on GetDescriptor(): %s",
             result.error().FormatDescription().c_str());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "HID device failed to get descriptor: %d", result->error_value());
      return result->error_value();
    }
    hid_report_desc_ = std::vector<uint8_t>(
        result.value()->data.data(), result.value()->data.data() + result.value()->data.count());
  }

  if (!info_.boot_protocol() || *info_.boot_protocol() == fhidbus::wire::HidBootProtocol::kNone) {
    return ZX_OK;
  }

  auto result = hidbus_.sync()->GetProtocol();
  if (!result.ok()) {
    zxlogf(ERROR, "FIDL transport failed on GetProtocol(): %s",
           result.error().FormatDescription().c_str());
    return result.status();
  }
  if (result->is_error()) {
    if (result->error_value() == ZX_ERR_NOT_SUPPORTED) {
      return ZX_OK;
    }
    return result->error_value();
  }
  auto protocol = result.value()->protocol;

  // Only continue if the device was put into the boot protocol.
  if (protocol != fhidbus::HidProtocol::kBoot) {
    return ZX_OK;
  }

  // If we are a boot protocol kbd, we need to use the right HID descriptor.
  if (*info_.boot_protocol() == fhidbus::wire::HidBootProtocol::kKbd) {
    size_t actual = 0;
    const uint8_t* boot_kbd_desc = get_boot_kbd_report_desc(&actual);
    hid_report_desc_.resize(actual);
    memcpy(hid_report_desc_.data(), boot_kbd_desc, actual);

    // Disable numlock
    uint8_t zero = 0;
    auto result =
        hidbus_.sync()->SetReport(static_cast<fuchsia_hardware_input::ReportType>(
                                      fuchsia_hardware_input::ReportType::kOutput),
                                  0, fidl::VectorView<uint8_t>::FromExternal(&zero, sizeof(zero)));
    if (!result.ok()) {
      zxlogf(ERROR, "FIDL transport failed on SetReport(): %s",
             result.error().FormatDescription().c_str());
    } else if (result->is_error()) {
      zxlogf(ERROR, "HID device failed to set report: %d", result->error_value());
    }
    // ignore failure for now
  }

  // If we are a boot protocol pointer, we need to use the right HID descriptor.
  if (*info_.boot_protocol() == fhidbus::wire::HidBootProtocol::kPointer) {
    size_t actual = 0;
    const uint8_t* boot_mouse_desc = get_boot_mouse_report_desc(&actual);

    hid_report_desc_.resize(actual);
    memcpy(hid_report_desc_.data(), boot_mouse_desc, actual);
  }

  return ZX_OK;
}

const char* HidDevice::GetName() { return name_.data(); }

void HidDevice::RemoveInstance(HidInstance& instance) {
  fbl::AutoLock _(&instance_lock_);
  instance_list_.erase(instance);
}

zx_status_t HidDevice::Bind() {
  auto result = hidbus_.sync()->Query();
  if (!result.ok()) {
    zxlogf(ERROR, "FIDL transport failed on Query(): %s",
           result.error().FormatDescription().c_str());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "HID device failed to query: %d", result->error_value());
    return result->error_value();
  }
  info_ = fidl::ToNatural(result.value()->info);

  snprintf(name_.data(), name_.size(), "hid-device-%03d", info_.dev_num().value_or(0));
  name_[ZX_DEVICE_NAME_MAX] = 0;

  if (zx_status_t status = SetReportDescriptor(); status != ZX_OK) {
    zxlogf(ERROR, "hid: could not retrieve HID report descriptor: %s",
           zx_status_get_string(status));
    return status;
  }

  if (zx_status_t status = ProcessReportDescriptor(); status != ZX_OK) {
    zxlogf(ERROR, "hid: could not parse hid report descriptor: %s", zx_status_get_string(status));
    return status;
  }

  std::vector<zx_device_str_prop_t> props;
  props.reserve(page_usage_.size());
  for (const auto& prop : page_usage_) {
    auto bind = FindProp(prop);
    if (bind.is_error()) {
      zxlogf(DEBUG, "Page %x Usage %x not supported as a bind property yet. Skipping.", prop.page,
             prop.usage);
      continue;
    }
    props.emplace_back(zx_device_str_prop_t{
        .key = kBindPropKeyMap[*bind].c_str(),
        .property_value = str_prop_bool_val(true),
    });
  }

  if (zx_status_t status = InitReassemblyBuffer(); status != ZX_OK) {
    zxlogf(ERROR, "hid: failed to initialize reassembly buffer: %s", zx_status_get_string(status));
    return status;
  }

  // TODO: delay calling start until we've been opened by someone
  {
    auto result = hidbus_.sync()->Start();
    if (!result.ok()) {
      zxlogf(ERROR, "FIDL transport failed on Start(): %s",
             result.error().FormatDescription().c_str());
      ReleaseReassemblyBuffer();
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "HID device failed to start: %d", result->error_value());
      ReleaseReassemblyBuffer();
      return result->error_value();
    }
  }

  {
    auto result = hidbus_.sync()->SetIdle(0, 0);
    if (!result.ok()) {
      zxlogf(ERROR, "FIDL transport failed on SetIdle(): %s",
             result.error().FormatDescription().c_str());
    } else if (result->is_error()) {
      zxlogf(ERROR, "HID device failed to set idle: %d", result->error_value());
    }
    // continue anyway
  }

  if (zx_status_t status =
          DdkAdd(ddk::DeviceAddArgs("hid-device").set_str_props(cpp20::span(props)));
      status != ZX_OK) {
    zxlogf(ERROR, "hid: device_add failed for HID device: %s", zx_status_get_string(status));
    ReleaseReassemblyBuffer();
    return status;
  }

  return ZX_OK;
}

static zx_status_t hid_bind(void* ctx, zx_device_t* parent) {
  auto client_end = ddk::Device<void>::DdkConnectFidlProtocol<fhidbus::Service::Device>(parent);
  if (!client_end.is_ok()) {
    zxlogf(ERROR, "Could not connect to FIDL %s", client_end.status_string());
    return client_end.error_value();
  }
  auto dev = std::make_unique<HidDevice>(parent, std::move(*client_end));

  auto status = dev->Bind();
  if (status == ZX_OK) {
    // devmgr is now in charge of the memory for dev.
    [[maybe_unused]] auto ptr = dev.release();
  }
  return status;
}

static zx_driver_ops_t hid_driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = hid_bind;
  return ops;
}();

}  // namespace hid_driver

// clang-format off
ZIRCON_DRIVER(hid, hid_driver::hid_driver_ops, "zircon", "0.1");

// clang-format on
