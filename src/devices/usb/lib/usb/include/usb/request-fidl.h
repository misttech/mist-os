// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_LIB_USB_INCLUDE_USB_REQUEST_FIDL_H_
#define SRC_DEVICES_USB_LIB_USB_INCLUDE_USB_REQUEST_FIDL_H_

#include <fidl/fuchsia.hardware.usb.request/cpp/fidl.h>
#include <lib/zx/bti.h>
#include <lib/zx/vmar.h>

#include <fbl/macros.h>

#include "src/devices/usb/lib/usb/include/usb/usb-request.h"

namespace usb {

// FidlRequest is a wrapper around a fuchsia_hardware_usb_request::Request implementing common
// functionality. Especially, FidlRequest keeps track of VMOs that were pinned upon PhysMap() and
// unpins them upon destruction.
class FidlRequest {
 public:
  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(FidlRequest);

  explicit FidlRequest(fuchsia_hardware_usb_request::Request request)
      : request_(std::move(request)) {}
  FidlRequest(FidlRequest&& request) = default;
  ~FidlRequest() { Unpin(); }

  // Pins VMOs if needed. For
  //  - fuchsia_hardware_usb_request::Buffer::Tag::kVmoId -- Uses preregistered VMO. Does nothing.
  //  - fuchsia_hardware_usb_request::Buffer::Tag::kVmo   -- Pins VMO.
  //  - fuchsia_hardware_usb_request::Buffer::Tag::kData  -- Creates, maps, pins, VMO. Copies data
  //                                                         to VMO. (Unmapped and unpinned on
  //                                                         Unpin()).
  zx_status_t PhysMap(const zx::bti& bti) {
    int64_t idx = -1;
    for (auto& d : *request_.data()) {
      idx++;
      zx_handle_t vmo_handle = ZX_HANDLE_INVALID;
      void* mapped;
      switch (d.buffer()->Which()) {
        case fuchsia_hardware_usb_request::Buffer::Tag::kVmoId:
          // Pre-registered and already pinned. Does not need to be pinned again.
          continue;
        case fuchsia_hardware_usb_request::Buffer::Tag::kVmo: {
          vmo_handle = d.buffer()->vmo()->get();
        } break;
        case fuchsia_hardware_usb_request::Buffer::Tag::kData: {
          // The price to pay for using fuchsia_hardware_usb_request::Buffer::Tag::kData instead of
          // VMOs is that a VMO needs to be created, mapped, pinned, data needs to be copied
          // to/from data buffer in both directions regardless of endpoint direction, cache needs to
          // be flushed, vmo then unpinned, and then unmapped.
          zx::vmo vmo;
          auto status = zx::vmo::create(*d.size(), 0, &vmo);
          if (status != ZX_OK) {
            return status;
          }

          status =
              zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo, *d.offset(),
                                         *d.size(), reinterpret_cast<uintptr_t*>(&mapped));
          if (status != ZX_OK) {
            return status;
          }

          memcpy(mapped, d.buffer()->data()->data(), *d.size());
          zx_cache_flush(&mapped, *d.size(), ZX_CACHE_FLUSH_DATA | ZX_CACHE_FLUSH_INVALIDATE);
          vmo_handle = vmo.release();
        } break;
        default:
          return ZX_ERR_NOT_SUPPORTED;
      }

      // Pin VMO.
      usb_request_t req = {
          .vmo_handle = vmo_handle,
          .size = *d.size(),
          .offset = *d.offset(),
          .pmt = ZX_HANDLE_INVALID,
          .phys_list = nullptr,
          .phys_count = 0,
      };
      // Abusing usb_request_physmap
      auto status = usb_request_physmap(&req, bti.get());
      if (status != ZX_OK) {
        return status;
      }
      pinned_vmos_[idx] = {req.pmt, req.phys_list, req.phys_count, mapped};
    }

    return ZX_OK;
  }

  // Unpins VMOs pinned by PhysMap.
  zx_status_t Unpin() {
    auto pinned_vmos = std::move(pinned_vmos_);
    for (const auto& [idx, pinned] : pinned_vmos) {
      if ((*request_.data())[idx].buffer()->Which() ==
          fuchsia_hardware_usb_request::Buffer::Tag::kData) {
        memcpy((*request_.data())[idx].buffer()->data()->data(), pinned.mapped,
               *(*request_.data())[idx].size());

        auto status = zx::vmar::root_self()->unmap(reinterpret_cast<uintptr_t>(pinned.mapped),
                                                   *(*request_.data())[idx].size());
        ZX_DEBUG_ASSERT(status == ZX_OK);
      }

      auto status = zx_pmt_unpin(pinned.pmt);
      ZX_DEBUG_ASSERT(status == ZX_OK);
      free(pinned.phys_list);
    }
    return ZX_OK;
  }

  // Gets ddk::PhysIter for the buffer at request.data()->at(idx)
  ddk::PhysIter phys_iter(size_t idx, size_t max_length) const {
    ZX_ASSERT(request_.data()->at(idx).size());
    ZX_ASSERT(pinned_vmos_.find(idx) != pinned_vmos_.end());
    auto length = *request_.data()->at(idx).size();
    auto offset = *request_.data()->at(idx).offset();
    static_assert(sizeof(phys_iter_sg_entry_t) == sizeof(sg_entry_t) &&
                  offsetof(phys_iter_sg_entry_t, length) == offsetof(sg_entry_t, length) &&
                  offsetof(phys_iter_sg_entry_t, offset) == offsetof(sg_entry_t, offset));
    phys_iter_buffer_t buf = {.phys = pinned_vmos_.at(idx).phys_list,
                              .phys_count = pinned_vmos_.at(idx).phys_count,
                              .length = length,
                              .vmo_offset = offset,
                              .sg_list = nullptr,
                              .sg_count = 0};
    return ddk::PhysIter(buf, max_length);
  }

  const fuchsia_hardware_usb_request::Request& request() const { return request_; }
  fuchsia_hardware_usb_request::Request take_request() {
    ZX_DEBUG_ASSERT(Unpin() == ZX_OK);
    return std::move(request_);
  }
  // Returns the total length of all data in the request. Saves to a variable for future use.
  size_t length() {
    if (!_length) {
      size_t len = 0;
      for (const auto& d : *request_.data()) {
        ZX_ASSERT(d.size());
        len += *d.size();
      }
      *_length = len;
    }
    return *_length;
  }

 private:
  // request_: FIDL request object.
  fuchsia_hardware_usb_request::Request request_;

  struct pinned_vmo_t {
    zx_handle_t pmt;
    uint64_t* phys_list;
    size_t phys_count;
    // mapped: only used for fuchsia_hardware_usb_request::Buffer::Tag::kData.
    void* mapped;
  };
  // pinned_vmos_: VMOs that were pinned by this request when PhysMap() was called. Will be unpinned
  // by when this request is destructed or when Unpin() is called. Indexed in the same order as
  // request_.data(), where only buffers that are fuchsia_hardware_usb_request::Buffer::Tag::kVmoId
  // are left empty.
  std::map<size_t, pinned_vmo_t> pinned_vmos_ = {};

  // _length: Total length saved so calculation doesn't have to be done multiple times.
  std::optional<size_t> _length = std::nullopt;
};

}  // namespace usb

#endif  // SRC_DEVICES_USB_LIB_USB_INCLUDE_USB_REQUEST_FIDL_H_
