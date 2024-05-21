// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "usb-hid.h"

#include <endian.h>
#include <fuchsia/hardware/usb/c/banjo.h>
#include <fuchsia/hardware/usb/cpp/banjo.h>
#include <fuchsia/hardware/usb/descriptor/c/banjo.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/sync/completion.h>
#include <stdlib.h>
#include <string.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <cmath>
#include <thread>

#include <fbl/auto_lock.h>
#include <pretty/hexdump.h>
#include <usb/hid.h>
#include <usb/usb-request.h>
#include <usb/usb.h>

namespace usb_hid {

namespace fhidbus = fuchsia_hardware_hidbus;

#define to_usb_hid(d) containerof(d, usb_hid_device_t, hiddev)

// This driver binds on any USB device that exposes HID reports. It passes the
// reports to the HID driver by implementing the HidBus protocol.

void UsbHidbus::UsbInterruptCallback(usb_request_t* req) {
  // TODO use usb request copyfrom instead of mmap
  void* buffer;
  zx_status_t status = usb_request_mmap(req, &buffer);
  if (status != ZX_OK) {
    zxlogf(ERROR, "usb-hid: usb_request_mmap failed: %s", zx_status_get_string(status));
    return;
  }
  zxlogf(TRACE, "usb-hid: callback request status %d", req->response.status);
  if (zxlog_level_enabled(TRACE)) {
    hexdump(buffer, req->response.actual);
  }

  bool requeue = true;
  switch (req->response.status) {
    case ZX_ERR_IO_NOT_PRESENT:
      requeue = false;
      break;
    case ZX_OK:
      if (start_) {
        binding_.ForEachBinding([&](const auto& binding) {
          auto result = fidl::WireSendEvent(binding)->OnReportReceived(
              fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(buffer),
                                                      req->response.actual),
              zx_clock_get_monotonic());
          if (!result.ok()) {
            zxlogf(ERROR, "OnReportReceived failed %s", result.error().FormatDescription().c_str());
          }
        });
      }
      break;
    default:
      zxlogf(ERROR, "usb-hid: unknown interrupt status %d; not requeuing req",
             req->response.status);
      requeue = false;
      break;
  }

  if (requeue) {
    usb_request_complete_callback_t complete = {
        .callback =
            [](void* ctx, usb_request_t* request) {
              static_cast<UsbHidbus*>(ctx)->UsbInterruptCallback(request);
            },
        .ctx = this,
    };
    usb_.RequestQueue(req, &complete);
  } else {
    req_queued_ = false;
  }
}

void UsbHidbus::Query(QueryCompleter::Sync& completer) { completer.ReplySuccess(info_); }

void UsbHidbus::Start(StartCompleter::Sync& completer) {
  start_++;

  if (!req_queued_) {
    req_queued_ = true;
    usb_request_complete_callback_t complete = {
        .callback =
            [](void* ctx, usb_request_t* request) {
              static_cast<UsbHidbus*>(ctx)->UsbInterruptCallback(request);
            },
        .ctx = this,
    };
    usb_.RequestQueue(req_, &complete);
  }
  completer.ReplySuccess();
}

void UsbHidbus::Stop(StopCompleter::Sync& completer) {
  start_--;
  // TODO(tkilbourn) when start reaches 0, set flag to stop requeueing the interrupt request when we
  // start using this callback
}

zx_status_t UsbHidbus::UsbHidControlIn(uint8_t req_type, uint8_t request, uint16_t value,
                                       uint16_t index, void* data, size_t length,
                                       size_t* out_length) {
  zx_status_t status;
  status = usb_.ControlIn(req_type, request, value, index, ZX_TIME_INFINITE,
                          reinterpret_cast<uint8_t*>(data), length, out_length);
  if (status == ZX_ERR_IO_REFUSED || status == ZX_ERR_IO_INVALID) {
    status = usb_.ResetEndpoint(0);
  }
  return status;
}

zx_status_t UsbHidbus::UsbHidControlOut(uint8_t req_type, uint8_t request, uint16_t value,
                                        uint16_t index, const void* data, size_t length,
                                        size_t* out_length) {
  zx_status_t status;
  status = usb_.ControlOut(req_type, request, value, index, ZX_TIME_INFINITE,
                           reinterpret_cast<const uint8_t*>(data), length);
  if (status == ZX_ERR_IO_REFUSED || status == ZX_ERR_IO_INVALID) {
    status = usb_.ResetEndpoint(0);
  }
  return status;
}

void UsbHidbus::GetDescriptor(fhidbus::wire::HidbusGetDescriptorRequest* request,
                              GetDescriptorCompleter::Sync& completer) {
  int desc_idx = -1;
  for (int i = 0; i < hid_desc_->bNumDescriptors; i++) {
    if (hid_desc_->descriptors[i].bDescriptorType == static_cast<uint16_t>(request->desc_type)) {
      desc_idx = i;
      break;
    }
  }
  if (desc_idx < 0) {
    completer.ReplyError(ZX_ERR_NOT_FOUND);
    return;
  }

  size_t desc_len = hid_desc_->descriptors[desc_idx].wDescriptorLength;
  std::vector<uint8_t> desc;
  desc.resize(desc_len);
  zx_status_t status =
      UsbHidControlIn(USB_DIR_IN | USB_TYPE_STANDARD | USB_RECIP_INTERFACE, USB_REQ_GET_DESCRIPTOR,
                      static_cast<uint16_t>(static_cast<uint16_t>(request->desc_type) << 8),
                      interface_, desc.data(), desc_len, nullptr);
  if (status < 0) {
    zxlogf(ERROR, "usb-hid: error reading report descriptor 0x%02x: %d",
           static_cast<uint16_t>(request->desc_type), status);
    completer.ReplyError(status);
    return;
  }
  completer.ReplySuccess(fidl::VectorView<uint8_t>::FromExternal(desc.data(), desc.size()));
}

void UsbHidbus::GetReport(fhidbus::wire::HidbusGetReportRequest* request,
                          GetReportCompleter::Sync& completer) {
  std::vector<uint8_t> report;
  report.resize(request->len);
  size_t actual;
  auto status = UsbHidControlIn(
      USB_DIR_IN | USB_TYPE_CLASS | USB_RECIP_INTERFACE, USB_HID_GET_REPORT,
      static_cast<uint16_t>(static_cast<uint16_t>(request->rpt_type) << 8 | request->rpt_id),
      interface_, report.data(), report.size(), &actual);
  if (status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }
  report.resize(actual);
  completer.ReplySuccess(fidl::VectorView<uint8_t>::FromExternal(report.data(), report.size()));
}

void UsbHidbus::SetReport(fhidbus::wire::HidbusSetReportRequest* request,
                          SetReportCompleter::Sync& completer) {
  if (has_endptout_) {
    sync_completion_reset(&set_report_complete_);
    usb_request_complete_callback_t complete = {
        .callback =
            [](void* ctx, usb_request_t* request) {
              sync_completion_signal(&static_cast<UsbHidbus*>(ctx)->set_report_complete_);
            },
        .ctx = this,
    };

    request_out_->header.length = request->data.count();
    if (request->data.count() > endptout_max_size_) {
      completer.ReplyError(ZX_ERR_BUFFER_TOO_SMALL);
      return;
    }
    size_t result =
        usb_request_copy_to(request_out_, request->data.data(), request->data.count(), 0);
    ZX_ASSERT(result == request->data.count());
    usb_.RequestQueue(request_out_, &complete);
    auto status = sync_completion_wait(&set_report_complete_, ZX_TIME_INFINITE);
    if (status != ZX_OK) {
      completer.ReplyError(status);
      return;
    }
    completer.ReplySuccess();
    return;
  }
  auto status = UsbHidControlOut(
      USB_DIR_OUT | USB_TYPE_CLASS | USB_RECIP_INTERFACE, USB_HID_SET_REPORT,
      (static_cast<uint16_t>(static_cast<uint16_t>(request->rpt_type) << 8 | request->rpt_id)),
      interface_, request->data.data(), request->data.count(), NULL);
  if (status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }
  completer.ReplySuccess();
}

void UsbHidbus::GetIdle(fhidbus::wire::HidbusGetIdleRequest* request,
                        GetIdleCompleter::Sync& completer) {
  uint8_t duration;
  auto status = UsbHidControlIn(USB_DIR_IN | USB_TYPE_CLASS | USB_RECIP_INTERFACE, USB_HID_GET_IDLE,
                                request->rpt_id, interface_, &duration, sizeof(duration), NULL);
  if (status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }
  completer.ReplySuccess(duration);
}

void UsbHidbus::SetIdle(fhidbus::wire::HidbusSetIdleRequest* request,
                        SetIdleCompleter::Sync& completer) {
  auto status = UsbHidControlOut(
      USB_DIR_OUT | USB_TYPE_CLASS | USB_RECIP_INTERFACE, USB_HID_SET_IDLE,
      static_cast<uint16_t>((request->duration << 8) | request->rpt_id), interface_, NULL, 0, NULL);
  if (status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }
  completer.ReplySuccess();
}

void UsbHidbus::GetProtocol(GetProtocolCompleter::Sync& completer) {
  uint8_t protocol;
  auto status =
      UsbHidControlIn(USB_DIR_IN | USB_TYPE_CLASS | USB_RECIP_INTERFACE, USB_HID_GET_PROTOCOL, 0,
                      interface_, &protocol, sizeof(protocol), NULL);
  if (status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }
  completer.ReplySuccess(static_cast<fhidbus::wire::HidProtocol>(protocol));
}

void UsbHidbus::SetProtocol(fhidbus::wire::HidbusSetProtocolRequest* request,
                            SetProtocolCompleter::Sync& completer) {
  auto status =
      UsbHidControlOut(USB_DIR_OUT | USB_TYPE_CLASS | USB_RECIP_INTERFACE, USB_HID_SET_PROTOCOL,
                       static_cast<uint8_t>(request->protocol), interface_, NULL, 0, NULL);
  if (status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }
  completer.ReplySuccess();
}

void UsbHidbus::DdkUnbind(ddk::UnbindTxn txn) {
  unbind_thread_ = std::thread([this, txn = std::move(txn)]() mutable {
    usb_.CancelAll(endptin_address_);
    if (has_endptout_) {
      usb_.CancelAll(endptout_address_);
    }
    txn.Reply();
  });
}

void UsbHidbus::DdkRelease() {
  if (req_) {
    usb_request_release(req_);
  }
  usb_desc_iter_release(&desc_iter_);
  unbind_thread_.join();
  delete this;
}

void UsbHidbus::FindDescriptors(usb::Interface interface, usb_hid_descriptor_t** hid_desc,
                                const usb_endpoint_descriptor_t** endptin,
                                const usb_endpoint_descriptor_t** endptout) {
  for (auto& descriptor : interface.GetDescriptorList()) {
    if (descriptor.b_descriptor_type == USB_DT_HID) {
      *hid_desc = (usb_hid_descriptor_t*)&descriptor;
    } else if (descriptor.b_descriptor_type == USB_DT_ENDPOINT) {
      if (usb_ep_direction((usb_endpoint_descriptor_t*)&descriptor) == USB_ENDPOINT_IN &&
          usb_ep_type((usb_endpoint_descriptor_t*)&descriptor) == USB_ENDPOINT_INTERRUPT) {
        *endptin = (usb_endpoint_descriptor_t*)&descriptor;
      } else if (usb_ep_direction((usb_endpoint_descriptor_t*)&descriptor) == USB_ENDPOINT_OUT &&
                 usb_ep_type((usb_endpoint_descriptor_t*)&descriptor) == USB_ENDPOINT_INTERRUPT) {
        *endptout = (usb_endpoint_descriptor_t*)&descriptor;
      }
    }
  }
}

zx_status_t UsbHidbus::Bind(ddk::UsbProtocolClient usbhid) {
  zx_status_t status;
  usb_ = usbhid;

  usb_device_descriptor_t device_desc;
  usb_.GetDeviceDescriptor(&device_desc);
  auto info_builder = fhidbus::wire::HidInfo::Builder(arena_);
  info_builder.vendor_id(le16toh(device_desc.id_vendor));
  info_builder.product_id(le16toh(device_desc.id_product));

  parent_req_size_ = usb_.GetRequestSize();
  status = usb::InterfaceList::Create(usb_, true, &usb_interface_list_);
  if (status != ZX_OK) {
    return status;
  }

  usb_hid_descriptor_t* hid_desc = NULL;
  const usb_endpoint_descriptor_t* endptin = NULL;
  const usb_endpoint_descriptor_t* endptout = NULL;
  auto interface = *usb_interface_list_->begin();

  FindDescriptors(interface, &hid_desc, &endptin, &endptout);
  if (!hid_desc) {
    status = ZX_ERR_NOT_SUPPORTED;
    return status;
  }
  if (!endptin) {
    status = ZX_ERR_NOT_SUPPORTED;
    return status;
  }
  hid_desc_ = hid_desc;
  endptin_address_ = endptin->b_endpoint_address;
  // Calculation according to 9.6.6 of USB2.0 Spec for interrupt endpoints
  switch (auto speed = usb_.GetSpeed()) {
    case USB_SPEED_LOW:
    case USB_SPEED_FULL:
      if (endptin->b_interval > 255 || endptin->b_interval < 1) {
        zxlogf(ERROR, "bInterval for LOW/FULL Speed EPs must be between 1 and 255. bInterval = %u",
               endptin->b_interval);
        return ZX_ERR_OUT_OF_RANGE;
      }
      info_builder.polling_rate(zx::msec(endptin->b_interval).to_usecs());
      break;
    case USB_SPEED_HIGH:
      if (endptin->b_interval > 16 || endptin->b_interval < 1) {
        zxlogf(ERROR, "bInterval for HIGH Speed EPs must be between 1 and 16. bInterval = %u",
               endptin->b_interval);
        return ZX_ERR_OUT_OF_RANGE;
      }
      info_builder.polling_rate(static_cast<uint64_t>(pow(2, endptin->b_interval - 1)) *
                                zx::usec(125).to_usecs());
      break;
    default:
      zxlogf(ERROR, "Unrecognized USB Speed %u", speed);
      return ZX_ERR_NOT_SUPPORTED;
  }

  if (endptout) {
    endptout_address_ = endptout->b_endpoint_address;
    has_endptout_ = true;
    endptout_max_size_ = usb_ep_max_packet(endptout);
    status = usb_request_alloc(&request_out_, endptout_max_size_, endptout->b_endpoint_address,
                               parent_req_size_);
  }

  interface_ = interface.descriptor()->b_interface_number;
  info_builder.dev_num(interface_);
  if (interface.descriptor()->b_interface_protocol == USB_HID_PROTOCOL_KBD) {
    info_builder.boot_protocol(fhidbus::wire::HidBootProtocol::kKbd);
  } else if (interface.descriptor()->b_interface_protocol == USB_HID_PROTOCOL_MOUSE) {
    info_builder.boot_protocol(fhidbus::wire::HidBootProtocol::kPointer);
  } else {
    info_builder.boot_protocol(fhidbus::wire::HidBootProtocol::kNone);
  }
  info_builder.version(0);
  info_ = info_builder.Build();

  status = usb_request_alloc(&req_, usb_ep_max_packet(endptin), endptin->b_endpoint_address,
                             parent_req_size_);
  if (status != ZX_OK) {
    status = ZX_ERR_NO_MEMORY;
    return status;
  }

  auto result = outgoing_.AddService<fhidbus::Service>(fhidbus::Service::InstanceHandler({
      .device = binding_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                       fidl::kIgnoreBindingClosure),
  }));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to add Hidbus protocol: %s", result.status_string());
    return result.status_value();
  }
  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    return endpoints.status_value();
  }
  result = outgoing_.Serve(std::move(endpoints->server));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to service the outgoing directory");
    return result.status_value();
  }

  std::array offers = {
      fhidbus::Service::Name,
  };
  status = DdkAdd(ddk::DeviceAddArgs("usb-hid").set_fidl_service_offers(offers).set_outgoing_dir(
      endpoints->client.TakeChannel()));
  if (status != ZX_OK) {
    zxlogf(ERROR, "DdkAdd failed: %d", status);
    return status;
  }

  return ZX_OK;
}

static zx_status_t usb_hid_bind(void* ctx, zx_device_t* parent) {
  auto usbHid = std::make_unique<UsbHidbus>(parent);

  ddk::UsbProtocolClient usb;
  zx_status_t status = device_get_protocol(parent, ZX_PROTOCOL_USB, &usb);

  if (status != ZX_OK) {
    return status;
  }

  status = usbHid->Bind(usb);
  if (status == ZX_OK) {
    // devmgr is now in charge of the memory for dev.
    [[maybe_unused]] auto ptr = usbHid.release();
  }
  return status;
}

static zx_driver_ops_t usb_hid_driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = usb_hid_bind;
  return ops;
}();

}  // namespace usb_hid

ZIRCON_DRIVER(usb_hid, usb_hid::usb_hid_driver_ops, "zircon", "0.1");
