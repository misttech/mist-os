// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_LIB_USB_INCLUDE_USB_INTERNAL_REQUEST_FIDL_H_
#define SRC_DEVICES_USB_LIB_USB_INCLUDE_USB_INTERNAL_REQUEST_FIDL_H_

#include <fidl/fuchsia.hardware.usb.request/cpp/fidl.h>

// Figure out which logging runtime and function to use, and bind it to the FDF_LOG() macro where
// necessary. The possible cases are:
//   1. DFv2 runtime using the comapt shim. To link against the compat shim, build with
//      -DDFV2_COMPAT_LOGGING.
//   2. DFv1 runtime using the DDK. To use link against the DDK, build with -DDFV1_LOGGING.
//   3. (default) DFv2 runtime using the native DFv2 logging runtime. With Neither flag in effect,
//      link against the native DFv2 runtime.
//
// If building against the SDK, the binary must be linked against the native DFv2 logging runtime as
// neither the DDK nor compat shim are baked into the SDK.
#ifdef DFV2_COMPAT_LOGGING
#include <lib/driver/compat/cpp/logging.h>  // nogncheck
#elif defined(DFV1_LOGGING)
#include <lib/ddk/debug.h>  // nogncheck
#define FDF_LOG(args, ...) zxlogf(args, __VA_ARGS__)
#else
#include "sdk/lib/driver/logging/cpp/logger.h"  // nogncheck
#endif

#include <lib/zx/bti.h>
#include <lib/zx/vmar.h>

#include <functional>
#include <map>
#include <mutex>
#include <optional>
#include <queue>
#include <utility>
#include <vector>

#include "src/devices/usb/lib/usb/align.h"

namespace usb::internal {

// EndpointType: One of 4 endpoint types as defined by USB.
enum EndpointType : uint8_t {
  UNKNOWN,

  CONTROL,
  BULK,
  ISOCHRONOUS,
  INTERRUPT,
};

// MappedVmo is a container that keeps track of the virtual address and size of the VMO mapped.
struct MappedVmo {
  // addr: virtual address.
  zx_vaddr_t addr;
  // size: size of VMO that was mapped to this virtual address.
  size_t size;
};

// FidlRequest is a wrapper around a fuchsia_hardware_usb_request::Request implementing common
// functionality. Especially, FidlRequest keeps track of VMOs that were pinned upon PhysMap() and
// unpins them upon destruction.
template <typename PhysIterType>
class FidlRequest {
 public:
  using get_mapped_func_t = std::function<zx::result<std::optional<MappedVmo>>(
      const fuchsia_hardware_usb_request::Buffer& buffer)>;

  FidlRequest() = default;
  explicit FidlRequest(EndpointType ep_type) {
    switch (ep_type) {
      case CONTROL:
        set_control();
        break;
      case BULK:
        set_bulk();
        break;
      case ISOCHRONOUS:
        set_isochronous();
        break;
      case INTERRUPT:
        set_interrupt();
        break;
      default:
        ZX_ASSERT(false);
    };
  }
  explicit FidlRequest(fuchsia_hardware_usb_request::Request request)
      : request_(std::move(request)) {}

  // Disallow copy and assign, allow move.
  FidlRequest(FidlRequest&& request) = default;
  FidlRequest& operator=(FidlRequest&& request) = delete;
  FidlRequest(const FidlRequest&) = delete;
  FidlRequest& operator=(const FidlRequest&) = delete;

  ~FidlRequest() { Unpin(); }

  FidlRequest& set_control(fuchsia_hardware_usb_descriptor::UsbSetup setup = {}) {
    request_.information(fuchsia_hardware_usb_request::RequestInfo::WithControl(
        fuchsia_hardware_usb_request::ControlRequestInfo().setup(std::move(setup))));
    return *this;
  }
  FidlRequest& set_bulk() {
    request_.information(fuchsia_hardware_usb_request::RequestInfo::WithBulk(
        fuchsia_hardware_usb_request::BulkRequestInfo()));
    return *this;
  }
  FidlRequest& set_isochronous(uint64_t frame_id = 0) {
    request_.information(fuchsia_hardware_usb_request::RequestInfo::WithIsochronous(
        fuchsia_hardware_usb_request::IsochronousRequestInfo().frame_id(frame_id)));
    return *this;
  }
  FidlRequest& set_interrupt() {
    request_.information(fuchsia_hardware_usb_request::RequestInfo::WithInterrupt(
        fuchsia_hardware_usb_request::InterruptRequestInfo()));
    return *this;
  }

  FidlRequest& add_vmo_id(uint64_t vmo_id, size_t size = 0, size_t offset = 0) {
    if (!request_.data().has_value()) {
      request_.data().emplace();
    }

    request_.data()
        ->emplace_back()
        .buffer(fuchsia_hardware_usb_request::Buffer::WithVmoId(vmo_id))
        .offset(offset)
        .size(size);
    return *this;
  }
  FidlRequest& add_data(std::vector<uint8_t> data = {}, size_t size = 0, size_t offset = 0) {
    if (!request_.data().has_value()) {
      request_.data().emplace();
    }

    request_.data()
        ->emplace_back()
        .buffer(fuchsia_hardware_usb_request::Buffer::WithData(std::move(data)))
        .offset(offset)
        .size(size);
    return *this;
  }
  FidlRequest& clear_buffers() {
    for (auto& d : *request_.data()) {
      d.offset(0);
      d.size(0);
      if (d.buffer()->Which() == fuchsia_hardware_usb_request::Buffer::Tag::kData) {
        d.buffer()->data()->clear();
      }
    }
    return *this;
  }
  FidlRequest& reset_buffers(const get_mapped_func_t& GetMapped) {
    for (auto& d : *request_.data()) {
      d.offset(0);
      switch (d.buffer()->Which()) {
        case fuchsia_hardware_usb_request::Buffer::Tag::kVmoId: {
          auto mapped = GetMapped(*d.buffer());
          ZX_ASSERT(mapped.is_ok());
          d.size(mapped->size);
        } break;
        case fuchsia_hardware_usb_request::Buffer::Tag::kData:
          d.size(fuchsia_hardware_usb_request::kMaxTransferSize);
          break;
        default:
          break;
      }
    }
    return *this;
  }

  // CopyTo: tries to copy `size` bytes from `buffer` to contiguous `request` buffers from
  // `offset`. Returns the number of bytes copied for each buffer.
  std::vector<size_t> CopyTo(size_t offset, const void* buffer, size_t size,
                             const get_mapped_func_t& GetMapped) {
    const uint8_t* start = static_cast<const uint8_t*>(buffer);
    size_t todo = size;
    size_t cur_offset = offset;
    std::vector<size_t> cp_sizes(request_.data()->size(), 0);
    int32_t i = -1;
    for (auto& d : *request_.data()) {
      // Accumulator accounting done at head of loop due to multiple exit logic paths.
      i++;
      auto mapped = GetMapped(*d.buffer());
      if (mapped.is_error()) {
        return cp_sizes;
      }

      uint8_t* addr;
      size_t buffer_size;
      if (!mapped.value()) {
        // Buffer is fuchsia_hardware_usb_request::Buffer::Tag::kData
        buffer_size =
            std::min(todo, static_cast<size_t>(fuchsia_hardware_usb_request::kMaxTransferSize));
        d.buffer()->data()->resize(buffer_size);
        addr = d.buffer()->data()->data();
      } else {
        buffer_size = mapped->size;
        addr = reinterpret_cast<uint8_t*>(mapped->addr);
      }

      if (cur_offset >= buffer_size) {
        cur_offset -= buffer_size;
        continue;
      }

      size_t cp_size = std::min(todo, buffer_size - cur_offset);
      memcpy(addr + cur_offset, start, cp_size);
      cur_offset = 0;
      start += cp_size;
      todo -= cp_size;
      cp_sizes[i] = cp_size;

      if (todo == 0) {
        // Break out early.
        break;
      }
    }

    return cp_sizes;
  }

  // CopyFrom: tries to copy `size` bytes from `request` (starting from `offset`) to `buffer`.
  // Returns the number of bytes copied for each buffer.
  std::vector<size_t> CopyFrom(size_t offset, void* buffer, size_t size,
                               const get_mapped_func_t& GetMapped) {
    uint8_t* start = static_cast<uint8_t*>(buffer);
    size_t todo = size;
    size_t cur_offset = offset;
    std::vector<size_t> cp_sizes(request_.data()->size(), 0);
    int32_t i = -1;
    for (const auto& d : *request_.data()) {
      // Accumulator accounting done at head of loop due to multiple exit logic paths.
      i++;
      if (cur_offset >= *d.size()) {
        cur_offset -= *d.size();
        continue;
      }

      auto mapped = GetMapped(*d.buffer());
      if (mapped.is_error()) {
        return cp_sizes;
      }

      zx_vaddr_t addr;
      if (!mapped.value()) {
        // Buffer is fuchsia_hardware_usb_request::Buffer::Tag::kData.
        addr = reinterpret_cast<zx_vaddr_t>(d.buffer()->data()->data());
      } else {
        addr = mapped->addr;
      }

      size_t cp_size = std::min(todo, *d.size() - cur_offset);
      memcpy(start, reinterpret_cast<uint8_t*>(addr) + cur_offset, cp_size);
      cur_offset = 0;
      start += cp_size;
      todo -= cp_size;
      cp_sizes[i] = cp_size;

      if (todo == 0) {
        // Break out early.
        break;
      }
    }

    return cp_sizes;
  }

  // About CacheFlush and CacheFlushInvalidate: CacheFlush or CacheFlush invalidate MUST always be
  // called after data is written to the VMO and before the data is processed on schedule (see
  // comment in endpoint.fidl on `RequestQueue`).
  // CacheFlush should be called for operations where data is to be written out, but not read in
  // and CacheFlushInvalidate should be called for operations where data needs to be read in.
  // CacheFlush and CacheFlushInvalidate flush and invalidate cache for all buffer regions of a
  // request.
  zx_status_t CacheFlushInvalidate(get_mapped_func_t GetMapped) {
    zx_status_t ret_status = ZX_OK;
    for (const auto& d : *request_.data()) {
      auto status = CacheHelper(d, ZX_CACHE_FLUSH_DATA | ZX_CACHE_FLUSH_INVALIDATE, GetMapped);
      if (status != ZX_OK) {
        // Return the latest failed status value, but keep trying to flush other buffer regions
        ret_status = status;
      }
    }
    return ret_status;
  }

  zx_status_t CacheFlush(get_mapped_func_t GetMapped) {
    zx_status_t ret_status = ZX_OK;
    for (const auto& d : *request_.data()) {
      auto status = CacheHelper(d, ZX_CACHE_FLUSH_DATA, GetMapped);
      if (status != ZX_OK) {
        // Return the latest failed status value, but keep trying to flush other buffer regions
        ret_status = status;
      }
    }
    return ret_status;
  }

  // CacheHelper flushes and invaldiates cache for specified buffer region.
  zx_status_t CacheHelper(const fuchsia_hardware_usb_request::BufferRegion& buffer,
                          uint32_t options, const get_mapped_func_t& GetMapped) {
    auto mapped = GetMapped(*buffer.buffer());
    if (mapped.is_error()) {
      return mapped.error_value();
    }
    if (!mapped.value()) {
      return ZX_OK;
    }

    auto status = zx_cache_flush(reinterpret_cast<void*>(mapped->addr), *buffer.size(), options);
    if (status != ZX_OK) {
      return status;
    }
    return ZX_OK;
  }

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
        case fuchsia_hardware_usb_request::Buffer::Tag::kData: {
          // The price to pay for using fuchsia_hardware_usb_request::Buffer::Tag::kData instead
          // of VMOs is that a VMO needs to be created, mapped, pinned, data needs to be copied
          // to/from data buffer in both directions regardless of endpoint direction, cache needs
          // to be flushed, vmo then unpinned, and then unmapped.
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

      // zx_bti_pin returns whole pages, so take into account unaligned vmo
      // offset and length when calculating the amount of pages returned
      uint64_t page_offset = USB_ROUNDDOWN(*d.offset(), kPageSize);
      // The buffer size is the vmo size from offset 0.
      uint64_t page_length = *d.size() - page_offset;
      uint64_t pages = USB_ROUNDUP(page_length, kPageSize) / kPageSize;

      std::unique_ptr<zx_paddr_t[]> paddrs{new zx_paddr_t[pages]};
      const size_t sub_offset = page_offset & (kPageSize - 1);
      const size_t pin_offset = page_offset - sub_offset;
      const size_t pin_length = USB_ROUNDUP(page_length + sub_offset, kPageSize);

      if (pin_length / kPageSize != pages) {
        return ZX_ERR_INVALID_ARGS;
      }
      zx_handle_t pmt;
      uint32_t options = ZX_BTI_PERM_READ | ZX_BTI_PERM_WRITE;
      zx_status_t status = zx_bti_pin(bti.get(), options, vmo_handle, pin_offset, pin_length,
                                      paddrs.get(), pages, &pmt);
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "zx_bti_pin(): %s", zx_status_get_string(status));
        return status;
      }

      // Account for the initial misalignment if any
      paddrs.get()[0] += sub_offset;

      pinned_vmos_[idx] = {
          pmt, paddrs.release(), pages, {reinterpret_cast<zx_vaddr_t>(mapped), *d.size()}};
    }

    return ZX_OK;
  }

  // Unpins VMOs pinned by PhysMap.
  zx_status_t Unpin() {
    auto pinned_vmos = std::move(pinned_vmos_);
    for (const auto& [idx, pinned] : pinned_vmos) {
      if ((*request_.data())[idx].buffer()->Which() ==
          fuchsia_hardware_usb_request::Buffer::Tag::kData) {
        memcpy((*request_.data())[idx].buffer()->data()->data(),
               reinterpret_cast<void*>(pinned.mapped.addr),
               std::min(*(*request_.data())[idx].size(), pinned.mapped.size));

        auto status = zx::vmar::root_self()->unmap(reinterpret_cast<uintptr_t>(pinned.mapped.addr),
                                                   pinned.mapped.size);
        ZX_DEBUG_ASSERT(status == ZX_OK);
      }

      auto status = zx_pmt_unpin(pinned.pmt);
      ZX_DEBUG_ASSERT(status == ZX_OK);
      delete[] pinned.phys_list;
    }
    return ZX_OK;
  }

  // Gets a PhysIterType for the buffer at request.data()->at(idx). PhysIterTypes return a
  // std::pair<zx_paddr_t, size_t> on iteration. For template specialization, see the public
  // headers outside the internal namespace.
  PhysIterType phys_iter(size_t idx, size_t max_length) const;

  fuchsia_hardware_usb_request::Request* operator->() { return &request_; }
  const fuchsia_hardware_usb_request::Request* operator->() const { return &request_; }
  const fuchsia_hardware_usb_request::Request& request() const { return request_; }
  // Ensures any removal of `request` is intentional and `Unpin` is called.
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
  const size_t kPageSize = zx_system_get_page_size();

  // request_: FIDL request object.
  fuchsia_hardware_usb_request::Request request_;

  struct pinned_vmo_t {
    zx_handle_t pmt;
    uint64_t* phys_list;
    size_t phys_count;
    // mapped: only used for fuchsia_hardware_usb_request::Buffer::Tag::kData.
    MappedVmo mapped;
  };
  // pinned_vmos_: VMOs that were pinned by this request when PhysMap() was called. Will be
  // unpinned by when this request is destructed or when Unpin() is called. Indexed in the same
  // order as request_.data(), where only buffers that are
  // fuchsia_hardware_usb_request::Buffer::Tag::kVmoId are left empty.
  std::map<size_t, pinned_vmo_t> pinned_vmos_ = {};

  // _length: Total length saved so calculation doesn't have to be done multiple times.
  std::optional<size_t> _length = std::nullopt;
};

// FidlRequestPool: pool of FidlRequests.
template <typename RequestType>
class FidlRequestPool {
 public:
  // When destructing, all of our requests should be sitting in the free list.
  ~FidlRequestPool() { ZX_DEBUG_ASSERT(free_reqs_.size() == size_); }

  // Add: called when adding a new request to the pool.
  void Add(RequestType&& request) {
    size_++;
    Put(std::move(request));
  }

  // Remove: called when removing a request from the pool.
  std::optional<RequestType> Remove() {
    auto req = Get();
    if (req.has_value()) {
      size_--;
    }
    return req;
  }

  std::optional<RequestType> Get() {
    std::lock_guard<std::mutex> _(mutex_);
    if (free_reqs_.empty()) {
      return std::nullopt;
    }

    auto req = std::move(free_reqs_.front());
    free_reqs_.pop();
    return std::move(req);
  }

  // Put: called when a request (originally obtained from `get`) is returned to the pool.
  void Put(RequestType&& request) {
    std::lock_guard<std::mutex> _(mutex_);
    free_reqs_.emplace(std::move(request));
    ZX_DEBUG_ASSERT(free_reqs_.size() <= size_);
  }

  bool Full() {
    std::lock_guard<std::mutex> _(mutex_);
    return free_reqs_.size() == size_;
  }
  bool Empty() {
    std::lock_guard<std::mutex> _(mutex_);
    return free_reqs_.empty();
  }

 private:
  std::mutex mutex_;
  std::queue<RequestType> free_reqs_ __TA_GUARDED(mutex_);

  std::atomic_uint32_t size_ = 0;
};

}  // namespace usb::internal

#endif  // SRC_DEVICES_USB_LIB_USB_INCLUDE_USB_INTERNAL_REQUEST_FIDL_H_
