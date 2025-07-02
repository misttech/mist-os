// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_ALLOCATION_H_
#define ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_ALLOCATION_H_

#include <lib/fit/result.h>
#include <lib/memalloc/pool.h>
#include <lib/memalloc/range.h>

#include <fbl/alloc_checker.h>
#include <ktl/byte.h>
#include <ktl/optional.h>
#include <ktl/span.h>
#include <ktl/string_view.h>
#include <ktl/utility.h>

// This object represents one memory allocation, and owns that allocation so
// destroying this object frees the allocation.  It acts as a smart pointer
// that also knows the size so it can deliver a raw pointer or a span<byte>.
class Allocation {
 public:
  // A default-constructed object is like a null pointer.
  // Allocation::New() must be called to create a non-null Allocation.
  Allocation() = default;

  Allocation(const Allocation&) = delete;

  Allocation(Allocation&& other) noexcept { *this = ktl::move(other); }

  Allocation& operator=(const Allocation&) = delete;

  Allocation& operator=(Allocation&& other) noexcept {
    ktl::swap(data_, other.data_);
    ktl::swap(alignment_, other.alignment_);
    ktl::swap(type_, other.type_);
    return *this;
  }

  ~Allocation() { reset(); }

  ktl::span<ktl::byte> data() const { return data_; }

  size_t size_bytes() const { return data_.size(); }

  auto get() const { return data_.data(); }

  // Gives the intended minimal alignment.
  size_t alignment() const { return alignment_; }

  memalloc::Type type() const { return type_; }

  void reset();

  // This returns the span like data() but transfers ownership like a move.
  [[nodiscard]] auto release() {
    auto result = data_;
    data_ = {};
    alignment_ = 0;
    type_ = memalloc::Type::kMaxAllocated;
    return result;
  }

  explicit operator bool() const { return !data_.empty(); }

  ktl::span<ktl::byte> operator*() const { return data_; }

  const ktl::span<ktl::byte>* operator->() const { return &data_; }

  void Resize(fbl::AllocChecker& ac, size_t new_size);

  // This must be called exactly once before using GetPool or New.
  static void Init(ktl::span<memalloc::Range> mem_ranges, ktl::span<memalloc::Range> special_ranges,
                   memalloc::Pool::AccessCallback access_callback = {});

  // Alternatively, this can be called instead of Init() to install a
  // previously-initialized memalloc::Pool that was handed off.
  static void InitWithPool(memalloc::Pool& pool);

  // If allocation fails, operator bool will return false later.
  // The AllocChecker must be checked after construction, too.
  static Allocation New(fbl::AllocChecker& ac, memalloc::Type type, size_t size,
                        size_t alignment = __STDCPP_DEFAULT_NEW_ALIGNMENT__,
                        ktl::optional<uint64_t> min_addr = ktl::nullopt,
                        ktl::optional<uint64_t> max_addr = ktl::nullopt);

  // Get the memalloc::Pool instance used to construct Allocation objects.
  // Every call returns the same object.  Note that a separate `#include
  // <lib/memalloc/pool.h>` is necessary to use the instance.
  [[gnu::const]] static memalloc::Pool& GetPool();

 private:
  ktl::span<ktl::byte> data_;
  size_t alignment_ = 0;
  memalloc::Type type_ = memalloc::Type::kMaxAllocated;
};

// Memory concept implementation over `Allocation` for `trivial_allocator::PageAllocator`.
class AllocationMemory {
 public:
  using Capability = Allocation;

  // Delegate to environment.
  size_t page_size() const;

  template <memalloc::Type MemoryType>
  std::pair<void*, Capability> Allocate(size_t size) {
    fbl::AllocChecker ac;
    auto alloc = Capability::New(ac, MemoryType, size);
    if (!ac.check()) {
      ZX_DEBUG_ASSERT(!alloc);
      return {};
    }
    ZX_DEBUG_ASSERT(alloc);
    return {alloc->data(), std::move(alloc)};
  }

  void Deallocate(Capability capability, void* address, size_t size) { capability.reset(); }

  void Release(Capability capability, void* address, size_t size) {
    ktl::ignore = capability.release();
  }
};

// Helper tor templating only on `Allocate` method.
template <memalloc::Type MemoryType>
class TypedMemoryAllocation : public AllocationMemory {
 public:
  std::pair<void*, Capability> Allocate(size_t size) {
    return AllocationMemory::template Allocate<MemoryType>(size);
  }
};

#endif  // ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_ALLOCATION_H_
