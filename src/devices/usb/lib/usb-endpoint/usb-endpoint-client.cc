// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/lib/usb-endpoint/include/usb-endpoint/usb-endpoint-client.h"

#include <lib/zx/vmar.h>

#include <functional>

namespace usb_endpoint::internal {

namespace {

zx::result<std::optional<uint64_t>> GetMappedKey(
    const fuchsia_hardware_usb_request::Buffer& buffer) {
  switch (buffer.Which()) {
    case fuchsia_hardware_usb_request::Buffer::Tag::kVmoId:
      return zx::ok(buffer.vmo_id().value());
    case fuchsia_hardware_usb_request::Buffer::Tag::kData:
      // Is not unmapped at this point.
      return zx::ok(std::nullopt);
    default:
      zxlogf(ERROR, "Unrecogonized buffer type %lu", static_cast<unsigned long>(buffer.Which()));
      return zx::error(ZX_ERR_INTERNAL);
  }
}

}  // namespace

UsbEndpointBase::~UsbEndpointBase() {
  // Note that VMOs should be unpinned when returned from drivers!
  while (auto req = free_reqs_.Remove()) {
    auto status = DeleteRequest(std::move(*req));
    if (status != ZX_OK) {
      zxlogf(ERROR, "Could not delete request %d", status);
    }
  }

  ZX_DEBUG_ASSERT(vmo_mapped_addrs_.empty());
}

zx_status_t UsbEndpointBase::Unmap(const fuchsia_hardware_usb_request::BufferRegion& buffer) {
  auto key = GetMappedKey(*buffer.buffer());
  if (key.is_error()) {
    return key.error_value();
  }
  if (!key.value()) {
    return ZX_OK;
  }

  if (vmo_mapped_addrs_.find(*key.value()) == vmo_mapped_addrs_.end()) {
    return ZX_OK;
  }

  auto mapped = vmo_mapped_addrs_.extract(*key.value()).mapped();
  auto status = zx::vmar::root_self()->unmap(mapped.addr, mapped.size);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to unmap VMO %d", status);
    return status;
  }

  return ZX_OK;
}

size_t UsbEndpointBase::AddRequests(size_t req_count, size_t size,
                                    fuchsia_hardware_usb_request::Buffer::Tag type) {
  switch (type) {
    case fuchsia_hardware_usb_request::Buffer::Tag::kVmoId:
      return RegisterVmos(req_count, size);
    case fuchsia_hardware_usb_request::Buffer::Tag::kData:
      for (size_t i = 0; i < req_count; i++) {
        free_reqs_.Add(std::move(usb::FidlRequest(ep_type_).add_data(std::vector<uint8_t>(size))));
      }
      return req_count;
    default:
      return 0;
  }
}

zx_status_t UsbEndpointBase::DeleteRequest(usb::FidlRequest&& request) {
  zx_status_t ret_status = ZX_OK;
  for (auto& d : *request->data()) {
    auto status = Unmap(d);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Could not unmap buffer region %d", status);
      // Return the latest failed status value, but keep trying to unmap the rest of the buffer
      // regions
      ret_status = status;
    }

    if (d.buffer()->Which() == fuchsia_hardware_usb_request::Buffer::Tag::kVmoId) {
      sync_completion_t wait;
      client_->UnregisterVmos(std::vector<uint64_t>{d.buffer()->vmo_id().value()})
          .Then([&wait](
                    fidl::Result<fuchsia_hardware_usb_endpoint::Endpoint::UnregisterVmos>& result) {
            if (result.is_error()) {
              zxlogf(ERROR, "Failed to unregister vmo %s",
                     result.error_value().FormatDescription().c_str());
            }
            sync_completion_signal(&wait);
          });
      sync_completion_wait(&wait, ZX_TIME_INFINITE);
    }
  }

  return ret_status;
}

size_t UsbEndpointBase::RegisterVmos(size_t vmo_count, size_t vmo_size) {
  std::vector<fuchsia_hardware_usb_endpoint::VmoInfo> vmo_info;
  for (uint32_t i = 0; i < vmo_count; i++) {
    vmo_info.emplace_back(
        std::move(fuchsia_hardware_usb_endpoint::VmoInfo().id(buffer_id_++).size(vmo_size)));
  }

  sync_completion_t wait;
  size_t actual = 0;
  client_->RegisterVmos(std::move(vmo_info))
      .Then([&](fidl::Result<fuchsia_hardware_usb_endpoint::Endpoint::RegisterVmos>& result) {
        if (result.is_error()) {
          zxlogf(ERROR, "Failed to register VMOs %s",
                 result.error_value().FormatDescription().c_str());
          sync_completion_signal(&wait);
          return;
        }

        actual = result->vmos().size();
        for (const auto& vmo : result->vmos()) {
          free_reqs_.Add(std::move(usb::FidlRequest(ep_type_).add_vmo_id(*vmo.id(), vmo_size)));

          zx_vaddr_t mapped_addr;
          auto status = zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0,
                                                   *vmo.vmo(), 0, vmo_size, &mapped_addr);
          if (status != ZX_OK) {
            zxlogf(ERROR, "Failed to map the vmo: %d", status);
            // Try for the next one.
            continue;
          }
          fbl::AutoLock _(&mutex_);
          vmo_mapped_addrs_.emplace(*vmo.id(), usb::MappedVmo{mapped_addr, vmo_size});
        }
        sync_completion_signal(&wait);
      });
  sync_completion_wait(&wait, ZX_TIME_INFINITE);

  return actual;
}

zx::result<std::optional<usb::MappedVmo>> UsbEndpointBase::get_mapped(
    const fuchsia_hardware_usb_request::Buffer& buffer) {
  auto key = GetMappedKey(buffer);
  if (key.is_error()) {
    return zx::error(key.error_value());
  }
  if (!key.value()) {
    return zx::ok(std::nullopt);
  }

  return zx::ok(vmo_mapped_addrs_.at(*key.value()));
}

}  // namespace usb_endpoint::internal
