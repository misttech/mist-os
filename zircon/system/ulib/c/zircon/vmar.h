// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_SYSTEM_ULIB_C_ZIRCON_VMAR_H_
#define ZIRCON_SYSTEM_ULIB_C_ZIRCON_VMAR_H_

#include <lib/zx/result.h>
#include <lib/zx/vmar.h>
#include <lib/zx/vmo.h>
#include <sys/uio.h>

#include <cstddef>
#include <span>
#include <utility>

#include "src/__support/macros/config.h"
#include "zircon_impl.h"

namespace LIBC_NAMESPACE_DECL {

// This is the VMAR to be used for general data allocations and mappings.
// **Note:** It should not be presumed to permit executable mappings.
inline zx::unowned_vmar AllocationVmar() { return zx::unowned_vmar{_zx_vmar_root_self()}; }

// This wraps size_t to ensure that a size is always rounded to whole pages.
class PageRoundedSize {
 public:
  constexpr PageRoundedSize() = default;
  constexpr PageRoundedSize(const PageRoundedSize&) = default;

  explicit PageRoundedSize(size_t raw_size) {
    if (raw_size > 0) {
      const size_t page_size = zx_system_get_page_size();
      rounded_size_ = (raw_size + page_size - 1) & -page_size;
    }
  }

  constexpr PageRoundedSize& operator=(const PageRoundedSize&) = default;

  constexpr PageRoundedSize& operator+=(PageRoundedSize other) {
    rounded_size_ += other.rounded_size_;
    return *this;
  }

  constexpr PageRoundedSize operator+(PageRoundedSize other) const {
    other += *this;
    return other;
  }

  constexpr bool operator==(const PageRoundedSize&) const = default;

  constexpr auto operator<=>(const PageRoundedSize&) const = default;

  constexpr size_t get() const { return rounded_size_; }

  explicit constexpr operator bool() const { return rounded_size_ > 0; }

  [[gnu::const]] static PageRoundedSize Page() {
    PageRoundedSize page_size;
    page_size.rounded_size_ = zx_system_get_page_size();
    return page_size;
  }

 private:
  size_t rounded_size_ = 0;
};

// This manages a VMO for use with GuardedPageBlock.  A single VMO is created
// to hold all the pages that will be mapped into separate blocks.
struct AllocationVmo {
  static zx::result<AllocationVmo> New(PageRoundedSize total_size) {
    AllocationVmo vmo;
    zx_status_t status = zx::vmo::create(total_size.get(), 0, &vmo.vmo);
    if (status != ZX_OK) [[unlikely]] {
      return zx::error{status};
    }
    return zx::ok(std::move(vmo));
  }

  uint64_t offset = 0;
  zx::vmo vmo;
};

// This describes a page-aligned block mapped inside a VMAR with guard regions.
// This is used for thread stacks, and for the thread area.
class GuardedPageBlock {
 public:
  constexpr GuardedPageBlock() = default;
  GuardedPageBlock(const GuardedPageBlock&) = delete;

  constexpr GuardedPageBlock(GuardedPageBlock&& other) noexcept
      : start_{std::exchange(other.start_, 0)}, size_{std::exchange(other.size_, {})} {}

  constexpr GuardedPageBlock& operator=(GuardedPageBlock&& other) noexcept {
    reset();
    start_ = std::exchange(other.start_, 0);
    size_ = std::exchange(other.size_, {});
    return *this;
  }

  // Allocate a guarded block by consuming the next pages of the VMO.
  // The returned span does not include the guard regions.
  // The generic template is implemented inline below.
  template <typename T = std::byte>
  zx::result<std::span<T>> Allocate(zx::unowned_vmar allocate_from, AllocationVmo& vmo,
                                    PageRoundedSize data_size, PageRoundedSize guard_below,
                                    PageRoundedSize guard_above);

  // The underlying implementation is out-of-line in this specialization.
  template <>
  zx::result<std::span<std::byte>> Allocate<std::byte>(  //
      zx::unowned_vmar allocate_from, AllocationVmo& vmo, PageRoundedSize data_size,
      PageRoundedSize guard_below, PageRoundedSize guard_above);

  void reset() {
    if (size_) {
      Unmap();
    }
  }

  ~GuardedPageBlock() { reset(); }

  size_t size_bytes() const { return size_.get(); }

  // This transfers ownership to the returned iovec.  This is only used for the
  // legacy glue with C code that uses iovec to store regions to be unmapped.
  iovec TakeIovec() && {
    return {.iov_base = reinterpret_cast<void*>(std::exchange(start_, 0)),
            .iov_len = std::exchange(size_, {}).get()};
  }

 private:
  void Unmap();

  uintptr_t start_ = 0;
  PageRoundedSize size_;
  zx::unowned_vmar vmar_;
};

// The real implementation is the std::byte specialization, out of line.
// Others just convert the pointer type.
template <typename T>
inline zx::result<std::span<T>> GuardedPageBlock::Allocate(  //
    zx::unowned_vmar allocate_from, AllocationVmo& vmo, PageRoundedSize data_size,
    PageRoundedSize guard_below, PageRoundedSize guard_above) {
  zx::result result =
      Allocate<std::byte>(allocate_from->borrow(), vmo, data_size, guard_below, guard_above);
  if (result.is_error()) {
    return result.take_error();
  }
  return zx::ok(std::span{
      reinterpret_cast<T*>(result->data()),
      result->size_bytes() / sizeof(T),
  });
}

}  // namespace LIBC_NAMESPACE_DECL

#endif  // ZIRCON_SYSTEM_ULIB_C_ZIRCON_VMAR_H_
