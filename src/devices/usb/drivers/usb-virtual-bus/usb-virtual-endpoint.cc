// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/usb-virtual-bus/usb-virtual-endpoint.h"

#include <fbl/auto_lock.h>

#include "src/devices/usb/drivers/usb-virtual-bus/usb-virtual-bus.h"

namespace usb_virtual_bus {

namespace {

inline usb_setup_t ToBanjo(fuchsia_hardware_usb_descriptor::UsbSetup setup) {
  return usb_setup_t{
      .bm_request_type = setup.bm_request_type(),
      .b_request = setup.b_request(),
      .w_value = setup.w_value(),
      .w_index = setup.w_index(),
      .w_length = setup.w_length(),
  };
}

}  // namespace

void UsbEpServer::OnFidlClosed(fidl::UnbindInfo) {
  for (const auto& [_, registered_vmo] : registered_vmos_) {
    auto status = zx::vmar::root_self()->unmap(registered_vmo.addr, registered_vmo.size);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to unmap VMO %d", status);
      continue;
    }
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
      zxlogf(ERROR, "VMO ID %lu already registered", id);
      continue;
    }

    zx::vmo vmo;
    auto status = zx::vmo::create(size, 0, &vmo);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to pin registered VMO %d", status);
      continue;
    }

    // Map VMO.
    zx_vaddr_t mapped_addr;
    status = zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo, 0, size,
                                        &mapped_addr);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to map the vmo: %d", status);
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
      zxlogf(ERROR, "Failed to unmap VMO %d", status);
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
    ep_->QueueRequest(usb::FidlRequest{std::move(req)});
  }
}

void UsbEpServer::CancelAll(CancelAllCompleter::Sync& completer) {
  completer.Reply(ep_->CancelAll());
}

void UsbEpServer::RequestComplete(zx_status_t status, size_t actual, usb::FidlRequest request) {
  auto defer_completion = *request->defer_completion();
  completions_.emplace_back(std::move(fuchsia_hardware_usb_endpoint::Completion()
                                          .request(request.take_request())
                                          .status(status)
                                          .transfer_size(actual)));
  if (defer_completion && status == ZX_OK) {
    return;
  }
  std::vector<fuchsia_hardware_usb_endpoint::Completion> completions;
  completions.swap(completions_);

  auto result = fidl::SendEvent(binding_)->OnCompletion(std::move(completions));
  if (result.is_error()) {
    zxlogf(ERROR, "Error sending event: %s", result.error_value().status_string());
  }
}

void UsbVirtualEp::QueueRequest(RequestVariant request) {
  bool connected;
  {
    fbl::AutoLock _(&bus_->connection_lock_);
    connected = bus_->connected_;
  }
  if (!connected) {
    RequestComplete(ZX_ERR_IO_NOT_PRESENT, 0, std::move(request));
    return;
  }

  fbl::AutoLock device_lock(&bus_->device_lock_);
  if (bus_->unbinding_) {
    device_lock.release();
    RequestComplete(ZX_ERR_IO_NOT_PRESENT, 0, std::move(request));
    return;
  }

  if (stalled) {
    device_lock.release();
    RequestComplete(ZX_ERR_IO_REFUSED, 0, std::move(request));
    return;
  }
  if (!is_control()) {
    host_reqs.push(std::move(request));
    bus_->process_requests_.Post(bus_->device_dispatcher_.async_dispatcher());
  } else {
    bus_->num_pending_control_reqs_++;
    // Control messages are a VERY special case.
    // They are synchronous; so we shouldn't dispatch them
    // to an I/O thread.
    // We can't hold a lock when responding to a control request
    device_lock.release();
    bus_->HandleControl(std::move(request));
  }
}

zx::result<> UsbVirtualEp::CancelAll() {
  std::queue<RequestVariant> q;
  {
    fbl::AutoLock lock(&bus_->device_lock_);
    q = std::move(host_reqs);
  }
  while (!q.empty()) {
    RequestComplete(ZX_ERR_IO, 0, std::move(q.front()));
    q.pop();
  }
  return zx::ok();
}

void UsbVirtualEp::RequestComplete(zx_status_t status, size_t actual, RequestVariant request) {
  if (std::holds_alternative<usb::BorrowedRequest<void>>(request)) {
    std::get<usb::BorrowedRequest<void>>(request).Complete(status, actual);
    return;
  }

  if (bus_->device_dispatcher() != fdf::Dispatcher::GetCurrent()->async_dispatcher()) {
    libsync::Completion wait;
    async::PostTask(bus_->device_dispatcher(), [this, status, actual, &request, &wait]() {
      bus_->host_->ep(index_)->RequestComplete(status, actual,
                                               std::move(std::get<usb::FidlRequest>(request)));
      wait.Signal();
    });
    wait.Wait();
  } else {
    bus_->host_->ep(index_)->RequestComplete(status, actual,
                                             std::move(std::get<usb::FidlRequest>(request)));
  }
}

void UsbVirtualBus::ProcessRequests() {
  struct complete_request_t {
    complete_request_t(UsbVirtualEp& ep, RequestVariant request, zx_status_t status, size_t actual)
        : ep(ep), request(std::move(request)), status(status), actual(actual) {}

    UsbVirtualEp& ep;
    RequestVariant request;
    zx_status_t status;
    size_t actual;
  };
  std::queue<complete_request_t> complete_reqs_pending;

  auto add_complete_reqs = [&complete_reqs_pending](UsbVirtualEp& ep, RequestVariant request,
                                                    zx_status_t status, size_t actual) {
    complete_reqs_pending.emplace(ep, std::move(request), status, actual);
  };
  auto process_complete_reqs = [&complete_reqs_pending]() {
    while (!complete_reqs_pending.empty()) {
      auto& complete = complete_reqs_pending.front();
      complete.ep.RequestComplete(complete.status, complete.actual, std::move(complete.request));
      complete_reqs_pending.pop();
    }
  };

  {
    fbl::AutoLock al(&device_lock_);
    bool has_work = true;
    while (has_work) {
      has_work = false;
      if (unbinding_) {
        for (auto& ep : eps_) {
          for (auto req = ep.device_reqs.pop(); req; req = ep.device_reqs.pop()) {
            add_complete_reqs(ep, std::move(req.value()), ZX_ERR_IO_NOT_PRESENT, 0);
          }
        }
        // We need to wait for all control requests to complete before completing the unbind.
        if (num_pending_control_reqs_ > 0) {
          complete_unbind_signal_.Wait(&device_lock_);
        }

        al.release();
        process_complete_reqs();

        // At this point, all pending control requests have been completed,
        // and any queued requests wil be immediately completed with an error.
        ZX_DEBUG_ASSERT(unbind_txn_.has_value());
        unbind_txn_->Reply();
        return;
      }
      // Data transfer between device/host (everything except ep 0)
      for (uint8_t i = 0; i < USB_MAX_EPS; i++) {
        auto& ep = eps_[i];
        while (!ep.host_reqs.empty() && !ep.device_reqs.is_empty()) {
          has_work = true;
          auto device_req = std::move(ep.device_reqs.pop().value());
          auto req = std::move(ep.host_reqs.front());
          ep.host_reqs.pop();
          size_t length = std::min(std::holds_alternative<usb::FidlRequest>(req)
                                       ? std::get<usb::FidlRequest>(req).length()
                                       : std::get<Request>(req).request()->header.length,
                                   device_req.request()->header.length);
          void* device_buffer;
          auto status = device_req.Mmap(&device_buffer);
          if (status != ZX_OK) {
            zxlogf(ERROR, "%s: usb_request_mmap failed: %d", __func__, status);
            add_complete_reqs(ep, std::move(req), status, 0);
            add_complete_reqs(ep, std::move(device_req), status, 0);
            continue;
          }

          if (i < IN_EP_START) {
            if (std::holds_alternative<usb::FidlRequest>(req)) {
              std::vector<size_t> result = std::get<usb::FidlRequest>(req).CopyFrom(
                  0, device_buffer, length,
                  fit::bind_member(host_->ep(i), &UsbEpServer::GetMapped));
              ZX_ASSERT(result.size() == 1);
              ZX_ASSERT(result[0] == length);
            } else {
              size_t result = std::get<Request>(req).CopyFrom(device_buffer, length, 0);
              ZX_ASSERT(result == length);
            }
          } else {
            if (std::holds_alternative<usb::FidlRequest>(req)) {
              std::vector<size_t> result = std::get<usb::FidlRequest>(req).CopyTo(
                  0, device_buffer, length,
                  fit::bind_member(host_->ep(i), &UsbEpServer::GetMapped));
              ZX_ASSERT(result.size() == 1);
              ZX_ASSERT(result[0] == length);
            } else {
              size_t result = std::get<Request>(req).CopyTo(device_buffer, length, 0);
              ZX_ASSERT(result == length);
            }
          }
          add_complete_reqs(ep, std::move(req), ZX_OK, length);
          add_complete_reqs(ep, std::move(device_req), ZX_OK, length);
        }
      }
    }
  }
  process_complete_reqs();
}

void UsbVirtualBus::HandleControl(RequestVariant req) {
  const usb_setup_t& setup =
      std::holds_alternative<usb::FidlRequest>(req)
          ? ToBanjo(*std::get<usb::FidlRequest>(req)->information()->control()->setup())
          : std::get<Request>(req).request()->setup;
  zx_status_t status;
  size_t length = le16toh(setup.w_length);
  size_t actual = 0;

  zxlogf(DEBUG, "%s type: 0x%02X req: %d value: %d index: %d length: %zu", __func__,
         setup.bm_request_type, setup.b_request, le16toh(setup.w_value), le16toh(setup.w_index),
         length);

  if (dci_intf_.is_valid()) {
    void* buffer = nullptr;

    if (length > 0) {
      if (std::holds_alternative<usb::FidlRequest>(req)) {
        auto& request_buffer = (*std::get<usb::FidlRequest>(req)->data())[0].buffer();
        const auto& buffer_type = request_buffer->Which();
        switch (buffer_type) {
          case fuchsia_hardware_usb_request::Buffer::Tag::kVmoId:
            buffer = reinterpret_cast<void*>(
                host_->ep(0)->registered_vmo(request_buffer->vmo_id().value()).addr);
            break;
          case fuchsia_hardware_usb_request::Buffer::Tag::kData:
            buffer = request_buffer->data()->data();
            break;
          default:
            zxlogf(ERROR, "%s: Unknown buffer type %u", __func__,
                   static_cast<uint32_t>(buffer_type));
            return;
        }
      } else {
        auto status = std::get<Request>(req).Mmap(&buffer);
        if (status != ZX_OK) {
          zxlogf(ERROR, "%s: usb_request_mmap failed: %d", __func__, status);
          eps_[0].RequestComplete(status, 0, std::move(req));
          return;
        }
      }
    }

    if ((setup.bm_request_type & USB_ENDPOINT_DIR_MASK) == USB_ENDPOINT_IN) {
      status = dci_intf_.Control(&setup, nullptr, 0, reinterpret_cast<uint8_t*>(buffer), length,
                                 &actual);
    } else {
      status = dci_intf_.Control(&setup, reinterpret_cast<uint8_t*>(buffer), length, nullptr, 0,
                                 nullptr);
    }
  } else {
    status = ZX_ERR_UNAVAILABLE;
  }

  {
    fbl::AutoLock device_lock(&device_lock_);
    num_pending_control_reqs_--;
    if (unbinding_ && num_pending_control_reqs_ == 0) {
      // The worker thread is waiting for the control request to complete.
      complete_unbind_signal_.Signal();
    }
  }

  eps_[0].RequestComplete(status, actual, std::move(req));
}

zx_status_t UsbVirtualBus::SetStall(uint8_t ep_address, bool stall) {
  uint8_t index = EpAddressToIndex(ep_address);
  if (index >= USB_MAX_EPS) {
    return ZX_ERR_INVALID_ARGS;
  }

  UsbVirtualEp& ep = eps_[index];
  std::optional<RequestVariant> req = std::nullopt;
  {
    fbl::AutoLock lock(&lock_);

    ep.stalled = stall;

    if (stall) {
      req.emplace(std::move(ep.host_reqs.front()));
      ep.host_reqs.pop();
    }
  }

  if (req) {
    ep.RequestComplete(ZX_ERR_IO_REFUSED, 0, std::move(*req));
  }

  return ZX_OK;
}

}  // namespace usb_virtual_bus
