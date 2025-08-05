// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/usb-virtual-bus/usb-virtual-endpoint.h"

#include "src/devices/usb/drivers/usb-virtual-bus/usb-virtual-bus.h"

namespace usb_virtual_bus {

namespace {

zx::result<void*> GetBuffer(
    RequestVariant& req,
    std::optional<usb::FidlRequest::get_mapped_func_t> mapping_fn = std::nullopt) {
  if (std::holds_alternative<Request>(req)) {
    void* buffer;
    auto status = std::get<Request>(req).Mmap(&buffer);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "usb_request_mmap failed: %d", status);
      return zx::error(status);
    }
    return zx::ok(buffer);
  }

  auto& request_buffer = (*std::get<usb::FidlRequest>(req)->data())[0].buffer();
  const auto& buffer_type = request_buffer->Which();
  switch (buffer_type) {
    case fuchsia_hardware_usb_request::Buffer::Tag::kVmoId:
      ZX_DEBUG_ASSERT(mapping_fn);
      {
        zx::result result = (*mapping_fn)(*request_buffer);
        if (result.is_error()) {
          FDF_LOG(ERROR, "Failed to map %s", result.status_string());
          return result.take_error();
        }
        return zx::ok(reinterpret_cast<void*>(result->addr));
      }
    case fuchsia_hardware_usb_request::Buffer::Tag::kData:
      return zx::ok(request_buffer->data()->data());
    default:
      FDF_LOG(ERROR, "%s: Unknown buffer type %u", __func__, static_cast<uint32_t>(buffer_type));
      return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
}

size_t GetLength(RequestVariant& req) {
  return std::holds_alternative<usb::FidlRequest>(req)
             ? std::get<usb::FidlRequest>(req).length()
             : std::get<Request>(req).request()->header.length;
}

fuchsia_hardware_usb_descriptor::UsbSetup GetSetup(RequestVariant& req) {
  if (std::holds_alternative<usb::FidlRequest>(req)) {
    return *std::get<usb::FidlRequest>(req)->information()->control()->setup();
  }

  usb_setup_t setup = std::get<Request>(req).request()->setup;
  return fuchsia_hardware_usb_descriptor::UsbSetup(setup.bm_request_type, setup.b_request,
                                                   setup.w_value, setup.w_index, setup.w_length);
}

}  // namespace

void UsbEpServer::Connect(fidl::ServerEnd<fuchsia_hardware_usb_endpoint::Endpoint> server_end) {
  if (binding_) {
    FDF_LOG(ERROR, "Endpoint already bound");
    return;
  }
  binding_.emplace(fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(server_end), this,
                   [this](fidl::UnbindInfo) {
                     for (const auto& [_, registered_vmo] : registered_vmos_) {
                       auto status =
                           zx::vmar::root_self()->unmap(registered_vmo.addr, registered_vmo.size);
                       if (status != ZX_OK) {
                         FDF_LOG(ERROR, "Failed to unmap VMO %d", status);
                         continue;
                       }
                     }
                   });
}

void UsbEpServer::QueueRequest(RequestVariant req) {
  if (ep_->stalled_) {
    // Do not process any requests if stalled.
    RequestComplete(ZX_ERR_IO_REFUSED, 0, req);
    return;
  }

  if (!ep_->is_control()) {
    pending_reqs_.push(std::move(req));
    ep_->process_requests_.Post(ep_->bus_->async_dispatcher());
  } else {
    ep_->HandleControl(std::move(req));
  }
}

void UsbEpServer::CommonCancelAll() {
  while (!pending_reqs_.empty()) {
    RequestComplete(ZX_ERR_IO_NOT_PRESENT, 0, pending_reqs_.front());
    pending_reqs_.pop();
  }
}

void UsbEpServer::RegisterVmos(RegisterVmosRequest& request,
                               RegisterVmosCompleter::Sync& completer) {
  std::vector<fuchsia_hardware_usb_endpoint::VmoHandle> vmos;
  for (const auto& info : request.vmo_ids()) {
    ZX_ASSERT(info.id());
    ZX_ASSERT(info.size());
    auto id = *info.id();
    auto size = *info.size();

    if (registered_vmos_.find(id) != registered_vmos_.end()) {
      FDF_LOG(ERROR, "VMO ID %lu already registered", id);
      continue;
    }

    zx::vmo vmo;
    auto status = zx::vmo::create(size, 0, &vmo);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to pin registered VMO %d", status);
      continue;
    }

    // Map VMO.
    zx_vaddr_t mapped_addr;
    status = zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo, 0, size,
                                        &mapped_addr);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to map the vmo: %d", status);
      // Try for the next one.
      continue;
    }

    // Save
    vmos.emplace_back(
        std::move(fuchsia_hardware_usb_endpoint::VmoHandle().id(id).vmo(std::move(vmo))));
    registered_vmos_.emplace(id, usb::MappedVmo{mapped_addr, size});
  }

  completer.Reply({std::move(vmos)});
}

void UsbEpServer::UnregisterVmos(UnregisterVmosRequest& request,
                                 UnregisterVmosCompleter::Sync& completer) {
  std::vector<zx_status_t> errors;
  std::vector<uint64_t> failed_vmo_ids;
  for (const auto& id : request.vmo_ids()) {
    auto registered_vmo = registered_vmos_.extract(id);
    if (registered_vmo.empty()) {
      failed_vmo_ids.emplace_back(id);
      errors.emplace_back(ZX_ERR_NOT_FOUND);
      continue;
    }

    auto status =
        zx::vmar::root_self()->unmap(registered_vmo.mapped().addr, registered_vmo.mapped().size);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to unmap VMO %d", status);
      failed_vmo_ids.emplace_back(id);
      errors.emplace_back(status);
      continue;
    }
  }
  completer.Reply({std::move(failed_vmo_ids), std::move(errors)});
}

void UsbEpServer::QueueRequests(QueueRequestsRequest& request,
                                QueueRequestsCompleter::Sync& completer) {
  for (auto& req : request.req()) {
    QueueRequest(usb::FidlRequest{std::move(req)});
  }
}

void UsbEpServer::CancelAll(CancelAllCompleter::Sync& completer) {
  CommonCancelAll();
  completer.Reply(zx::ok());
}

void UsbEpServer::RequestComplete(zx_status_t status, size_t actual, RequestVariant& request) {
  if (std::holds_alternative<usb::BorrowedRequest<void>>(request)) {
    std::get<usb::BorrowedRequest<void>>(request).Complete(status, actual);
    return;
  }

  auto& freq = std::get<usb::FidlRequest>(request);
  auto defer_completion = *freq->defer_completion();
  completions_.emplace_back(std::move(fuchsia_hardware_usb_endpoint::Completion()
                                          .request(freq.take_request())
                                          .status(status)
                                          .transfer_size(actual)));
  if (defer_completion && status == ZX_OK) {
    return;
  }
  std::vector<fuchsia_hardware_usb_endpoint::Completion> completions;
  completions.swap(completions_);

  auto result = fidl::SendEvent(*binding_)->OnCompletion(std::move(completions));
  if (result.is_error()) {
    FDF_LOG(ERROR, "Error sending event: %s", result.error_value().status_string());
  }
}

void UsbVirtualEp::ProcessRequests() {
  ZX_DEBUG_ASSERT(!is_control());

  if (!bus_->connected_) {
    // Do not process any requests if not connected.
    return;
  }

  struct complete_request_t {
    complete_request_t(RequestVariant request, zx_status_t status, size_t actual)
        : request(std::move(request)), status(status), actual(actual) {}

    RequestVariant request;
    zx_status_t status;
    size_t actual;
  };
  std::queue<complete_request_t> host_complete_reqs_pending;
  std::queue<complete_request_t> device_complete_reqs_pending;

  // Data transfer between device/host
  while (!host_.pending_reqs_.empty() && !device_.pending_reqs_.empty()) {
    auto device_req = std::move(device_.pending_reqs_.front());
    device_.pending_reqs_.pop();
    auto host_req = std::move(host_.pending_reqs_.front());
    host_.pending_reqs_.pop();

    size_t length = std::min(GetLength(host_req), GetLength(device_req));
    zx::result host_buffer = GetBuffer(host_req, fit::bind_member(&host_, &UsbEpServer::GetMapped));
    if (host_buffer.is_error()) {
      FDF_LOG(ERROR, "Failed to get host buffer %s", host_buffer.status_string());
      host_complete_reqs_pending.emplace(std::move(host_req), host_buffer.error_value(), 0);
      device_complete_reqs_pending.emplace(std::move(device_req), host_buffer.error_value(), 0);
      return;
    }
    zx::result device_buffer =
        GetBuffer(device_req, fit::bind_member(&device_, &UsbEpServer::GetMapped));
    if (device_buffer.is_error()) {
      FDF_LOG(ERROR, "Failed to get device buffer %s", device_buffer.status_string());
      host_complete_reqs_pending.emplace(std::move(host_req), device_buffer.error_value(), 0);
      device_complete_reqs_pending.emplace(std::move(device_req), device_buffer.error_value(), 0);
      return;
    }

    if (is_out()) {
      std::memcpy(*device_buffer, *host_buffer, length);
    } else {
      std::memcpy(*host_buffer, *device_buffer, length);
    }
    host_complete_reqs_pending.emplace(std::move(host_req), ZX_OK, length);
    device_complete_reqs_pending.emplace(std::move(device_req), ZX_OK, length);
  }

  while (!host_complete_reqs_pending.empty()) {
    auto& complete = host_complete_reqs_pending.front();
    host_.RequestComplete(complete.status, complete.actual, complete.request);
    host_complete_reqs_pending.pop();
  }

  while (!device_complete_reqs_pending.empty()) {
    auto& complete = device_complete_reqs_pending.front();
    device_.RequestComplete(complete.status, complete.actual, complete.request);
    device_complete_reqs_pending.pop();
  }
}

void UsbVirtualEp::HandleControl(RequestVariant req) {
  ZX_DEBUG_ASSERT(is_control());

  if (!bus_->connected_) {
    // Do not process any requests if not connected.
    host_.RequestComplete(ZX_ERR_IO_NOT_PRESENT, 0, req);
    return;
  }

  fuchsia_hardware_usb_descriptor::UsbSetup setup = GetSetup(req);

  FDF_LOG(DEBUG, "%s type: 0x%02X req: %d value: %d index: %d length: %hu", __func__,
          setup.bm_request_type(), setup.b_request(), setup.w_value(), setup.w_index(),
          setup.w_length());

  if (!bus_->dci_intf_.is_valid()) {
    FDF_LOG(ERROR, "Dci Interface not ready");
    host_.RequestComplete(ZX_ERR_UNAVAILABLE, 0, req);
    return;
  }

  const bool is_in = ((setup.bm_request_type() & USB_ENDPOINT_DIR_MASK) == USB_ENDPOINT_IN);
  std::vector<uint8_t> write(0);
  if (!is_in && setup.w_length() > 0) {
    zx::result result = GetBuffer(req, fit::bind_member(&host_, &UsbEpServer::GetMapped));
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to get buffer pointer %s", result.status_string());
      host_.RequestComplete(result.error_value(), 0, req);
      return;
    }
    write = std::vector<uint8_t>(reinterpret_cast<uint8_t*>(*result),
                                 reinterpret_cast<uint8_t*>(*result) + setup.w_length());
  }

  bus_->dci_intf_->Control({setup, std::move(write)})
      .Then([this, req = std::move(req), length = setup.w_length()](
                fidl::Result<fuchsia_hardware_usb_dci::UsbDciInterface::Control>& result) mutable {
        if (result.is_error()) {
          host_.RequestComplete(result.error_value().is_framework_error()
                                    ? ZX_ERR_IO_NOT_PRESENT
                                    : result.error_value().domain_error(),
                                0, req);
          return;
        }
        if (result->read().size() > length) {
          FDF_LOG(ERROR, "Buffer overflow!");
          host_.RequestComplete(ZX_ERR_NO_MEMORY, 0, req);
          return;
        }

        if (std::holds_alternative<usb::FidlRequest>(req) &&
            !std::get<usb::FidlRequest>(req)->data()) {
          // Means we should create a buffer for it to return.
          std::get<usb::FidlRequest>(req)->data().emplace();
          std::get<usb::FidlRequest>(req)
              ->data()
              ->emplace_back()
              .buffer(fuchsia_hardware_usb_request::Buffer::WithData(
                  std::vector<uint8_t>(result->read().size())))
              .offset(0)
              .size(result->read().size());
        }

        zx::result buffer = GetBuffer(req, fit::bind_member(&host_, &UsbEpServer::GetMapped));
        if (buffer.is_error()) {
          FDF_LOG(ERROR, "Failed to get buffer pointer %s", buffer.status_string());
          host_.RequestComplete(buffer.error_value(), 0, req);
          return;
        }
        std::memcpy(*buffer, result->read().data(), result->read().size());
        host_.RequestComplete(ZX_OK, result->read().size(), req);
      });
}

zx::result<> UsbVirtualEp::SetStall(bool stall) {
  stalled_ = stall;

  if (stall) {
    host_.RequestComplete(ZX_ERR_IO_REFUSED, 0, host_.pending_reqs_.front());
    host_.pending_reqs_.pop();
  }

  return zx::ok();
}

}  // namespace usb_virtual_bus
