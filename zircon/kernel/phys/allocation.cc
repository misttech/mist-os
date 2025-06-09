// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "phys/allocation.h"

#include <lib/fit/result.h>
#include <lib/memalloc/pool.h>
#include <zircon/assert.h>
#include <zircon/limits.h>

#include <ktl/byte.h>
#include <ktl/string_view.h>
#include <ktl/utility.h>
#include <phys/main.h>

#include <ktl/enforce.h>

namespace {

memalloc::Pool* gAllocationPool;

}  // namespace

// Global memory allocation book-keeping.
memalloc::Pool& Allocation::GetPool() {
  ZX_DEBUG_ASSERT(gAllocationPool);
  return *gAllocationPool;
}

void Allocation::InitWithPool(memalloc::Pool& pool) {
  ZX_DEBUG_ASSERT(!gAllocationPool);
  gAllocationPool = &pool;
}

// This is where actual allocation happens.
// The returned object is default-constructed if it fails.
Allocation Allocation::New(fbl::AllocChecker& ac, memalloc::Type type, size_t size,
                           size_t alignment, ktl::optional<uint64_t> min_addr,
                           ktl::optional<uint64_t> max_addr) {
  Allocation alloc;
  fit::result<fit::failed, uint64_t> result =
      GetPool().Allocate(type, size, alignment, min_addr, max_addr);
  ac.arm(size, result.is_ok());
  if (result.is_ok()) {
    alloc.data_ = {reinterpret_cast<ktl::byte*>(result.value()), size};
    alloc.alignment_ = alignment;
    alloc.type_ = type;
  }
  return alloc;
}

// This is where actual deallocation happens.  The destructor just calls this.
void Allocation::reset() {
  if (!data_.empty()) {
    auto result = GetPool().Free(reinterpret_cast<uint64_t>(data_.data()), data_.size());
    ZX_ASSERT(result.is_ok());
    data_ = {};
  }
}

void Allocation::Resize(fbl::AllocChecker& ac, size_t new_size) {
  ZX_ASSERT(!data_.empty());
  ZX_ASSERT(new_size > 0);
  ZX_ASSERT(type_ != memalloc::Type::kMaxAllocated);

  if (new_size == size_bytes()) {
    return;
  }

  const memalloc::Range range = {
      .addr = reinterpret_cast<uint64_t>(get()),
      .size = size_bytes(),
      .type = type_,
  };
  auto result = GetPool().Resize(range, new_size, alignment_);
  ac.arm(new_size, result.is_ok());
  if (result.is_ok()) {
    auto* new_addr = reinterpret_cast<ktl::byte*>(ktl::move(result).value());
    if (new_addr != get()) {
      memmove(new_addr, get(), size_bytes());
    }
    data_ = {new_addr, new_size};
  }
}

size_t AllocationMemory::page_size() const { return ZX_PAGE_SIZE; }
