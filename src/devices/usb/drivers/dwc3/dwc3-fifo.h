// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_DWC3_DWC3_FIFO_H_
#define SRC_DEVICES_USB_DRIVERS_DWC3_DWC3_FIFO_H_

#include <lib/dma-buffer/buffer.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/zx/result.h>

namespace dwc3 {

static inline const uint32_t kBufferSize = zx_system_get_page_size();

// A dma_buffer::ContiguousBuffer is cached, but leaves cache management to the user. These methods
// wrap zx_cache_flush with sensible boundary checking and validation.
zx_status_t CacheFlush(dma_buffer::ContiguousBuffer* buffer, zx_off_t offset, size_t length);
zx_status_t CacheFlushInvalidate(dma_buffer::ContiguousBuffer* buffer, zx_off_t offset,
                                 size_t length);

template <typename T>
class Fifo {
 public:
  virtual zx::result<> Init(zx::bti& bti) {
    if (!buffer_) {
      zx_status_t status =
          dma_buffer::CreateBufferFactory()->CreateContiguous(bti, kBufferSize, 12, true, &buffer_);
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "dma_buffer init fails: %s", zx_status_get_string(status));
        return zx::error(status);
      }

      first_ = static_cast<T*>(buffer_->virt());
      last_ = first_ + (kBufferSize / sizeof(T));
    }

    write_ = first_;
    read_ = write_;
    return zx::ok();
  }
  void Clear() { read_ = write_; }
  void Release() {
    first_ = write_ = read_ = last_ = nullptr;
    buffer_.reset();
  }

  std::vector<T> Read(T*& ptr, size_t count) {
    ZX_DEBUG_ASSERT((ptr >= first_) && (ptr <= last_));
    // invalidate cache so we can read fresh events
    const zx_off_t offset = (ptr - first_) * sizeof(T);
    const size_t todo = std::min<size_t>(last_ - ptr, count);
    std::vector<T> values(count);
    CacheFlushInvalidate(buffer_.get(), offset, todo * sizeof(T));
    memcpy(values.data(), ptr, todo * sizeof(T));
    if (count > todo) {
      CacheFlushInvalidate(buffer_.get(), 0, (count - todo) * sizeof(T));
      memcpy(values.data() + todo, first_, (count - todo) * sizeof(T));
    };

    return std::move(values);
  }

  zx_paddr_t Write(T*& ptr, size_t count = 1) {
    ZX_DEBUG_ASSERT((ptr >= first_) && (ptr <= last_));
    // invalidate cache so we can read fresh events
    const zx_off_t offset = (ptr - first_) * sizeof(T);
    const size_t todo = std::min<size_t>(last_ - ptr, count);
    CacheFlush(buffer_.get(), offset, todo * sizeof(T));
    if (count > todo) {
      CacheFlush(buffer_.get(), 0, (count - todo) * sizeof(T));
    }
    return GetPhys(ptr);
  }

  T* Advance(T*& ptr, size_t count = 1) {
    T* cur = ptr;
    ptr += count;
    if (ptr >= last_) {
      ptr = ptr - last_ + first_;
    }
    return cur;
  }

 protected:
  zx_paddr_t GetPhys(T* ptr) const {
    ZX_DEBUG_ASSERT((ptr >= first_) && (ptr <= last_));
    return buffer_->phys() + ((ptr - first_) * sizeof(T));
  }

  std::unique_ptr<dma_buffer::ContiguousBuffer> buffer_;

  T* first_{nullptr};  // first slot in the fifo
  T* write_{nullptr};  // next free write slot in the fifo
  T* read_{nullptr};   // next read slot in the fifo
  T* last_{nullptr};   // last slot in the fifo
};

}  // namespace dwc3

#endif  // SRC_DEVICES_USB_DRIVERS_DWC3_DWC3_FIFO_H_
