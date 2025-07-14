// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "object/io_buffer_shared_region_dispatcher.h"

#include <fbl/algorithm.h>

// static
zx_status_t IoBufferSharedRegionDispatcher::Create(
    uint64_t size, KernelHandle<IoBufferSharedRegionDispatcher>* handle, zx_rights_t* rights) {
  fbl::RefPtr<VmObjectPaged> vmo;
  if (zx_status_t status =
          VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, VmObjectPaged::kAlwaysPinned, size, &vmo);
      status != ZX_OK) {
    return status;
  }

  fbl::RefPtr<VmAddressRegion> kernel_vmar =
      VmAspace::kernel_aspace()->RootVmar()->as_vm_address_region();
  zx::result<VmAddressRegion::MapResult> mapping = kernel_vmar->CreateVmMapping(
      0, size, 0, 0, vmo, 0, ARCH_MMU_FLAG_PERM_READ | ARCH_MMU_FLAG_PERM_WRITE,
      "IOBuffer shared region");
  if (mapping.is_error()) {
    return mapping.status_value();
  }

  if (zx_status_t status = mapping->mapping->MapRange(0, size, /*commit=*/true); status != ZX_OK) {
    return status;
  }

  fbl::AllocChecker ac;
  *handle = KernelHandle(fbl::AdoptRef(new (&ac) IoBufferSharedRegionDispatcher(
      ktl::move(vmo), ktl::move(mapping->mapping), mapping->base)));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  *rights = default_rights();
  return ZX_OK;
}

zx::result<> IoBufferSharedRegionDispatcher::Write(const uint64_t tag, user_in_iovec_t message) {
  // 8 bytes for the tag, plus 8 bytes for the length.
  constexpr size_t kHeaderSize = 16;

  // The maximum is chosen so that an entire message including the message header fits within 64
  // KiB, and the total length fits within a 16 bit integer. This is sufficient for current use
  // cases, but can be increased if necessary.
  constexpr size_t kMaxMessageSize = (1 << 16) - 1 - kHeaderSize;

  // We use `char` here because that's what `user_in_iovec_t` uses.
  struct Vector {
    user_in_ptr<const char> data = user_in_ptr<const char>(nullptr);
    size_t len;
  } vectors[8];
  size_t vector_count = 0;
  size_t message_size = 0;

  // Copy the vectors to our stack copy so we can compute the message size and have access to the
  // vectors below once we have taken the lock.
  if (zx_status_t status = message.ForEach([&](user_in_ptr<const char> data, size_t len) {
        if (vector_count == std::size(vectors)) {
          return ZX_ERR_INVALID_ARGS;
        }
        if (kMaxMessageSize - message_size < len) {
          return ZX_ERR_INVALID_ARGS;
        }
        message_size += len;
        vectors[vector_count] = Vector{
            .data = data,
            .len = len,
        };
        ++vector_count;
        return ZX_ERR_NEXT;
      });
      status != ZX_OK) {
    return zx::error(status);
  }

  const uint64_t rounded_message_size = fbl::round_up(message_size + kHeaderSize, 8u);
  const uint64_t buffer_size = vmo_->size() - PAGE_SIZE;

  if (rounded_message_size > buffer_size) {
    return zx::error(ZX_ERR_NO_SPACE);
  }

  Header* header = GetHeader();
  ZX_ASSERT(header != nullptr);

  auto get_ptr = [=](uint64_t offset) {
    return &reinterpret_cast<char*>(header)[PAGE_SIZE + offset];
  };

  for (;;) {
    ktl::optional<UserCopyCaptureFaultsResult::FaultInfo> fault;

    {
      Guard<CriticalMutex> guard{get_lock()};

      // Relaxed ordering because only the kernel modifies this value while holding the dispatcher
      // lock.
      const uint64_t head = header->head.load(ktl::memory_order_relaxed);

      // Acquire ordering so that the following writes are not reordered before this load since
      // otherwise it is possible that we will overwrite a message that userspace has not finished
      // reading yet.
      const uint64_t tail = header->tail.load(ktl::memory_order_acquire);

      if (tail > head || ktl::numeric_limits<uint64_t>::max() - head < rounded_message_size ||
          !IS_ROUNDED(head, 8)) {
        return zx::error(ZX_ERR_IO_DATA_INTEGRITY);
      }

      if (head - tail > buffer_size - rounded_message_size) {
        return zx::error(ZX_ERR_NO_SPACE);
      }

      uint64_t offset = head % buffer_size;
      *reinterpret_cast<uint64_t*>(get_ptr(offset)) = tag;
      offset = (offset + 8) % buffer_size;

      *reinterpret_cast<uint64_t*>(get_ptr(offset)) = static_cast<uint64_t>(message_size);
      offset = (offset + 8) % buffer_size;

      for (size_t i = 0; i < vector_count && !fault; ++i) {
        user_in_ptr<const char> data = vectors[i].data;
        size_t len = vectors[i].len;

        while (len > 0) {
          const uint64_t amount = ktl::min(buffer_size - offset, len);

          UserCopyCaptureFaultsResult copy_result = arch_copy_from_user_capture_faults(
              get_ptr(offset), data.get(), amount, CopyContext::kBlockingNotAllowed);

          if (copy_result.status != ZX_OK) {
            if (copy_result.fault_info) {
              fault = copy_result.fault_info;
              break;
            } else {
              return zx::error(copy_result.status);
            }
          }

          offset = (offset + amount) % buffer_size;
          len -= amount;
          data = data.byte_offset(amount);
        }
      }

      if (!fault) {
        // Release ordering so that the previous writes are not reordered after this store since
        // otherwise it is possible for userspace to read old data when it notices the change in
        // head value.
        header->head.fetch_add(rounded_message_size, ktl::memory_order_release);
        UpdateStateLocked(0, 0, ZX_IOB_SHARED_REGION_UPDATED);
        return zx::ok();
      }
    }

    // Handle the fault and then retry.
    if (zx_status_t status = Thread::Current::SoftFault(fault->pf_va, fault->pf_flags);
        status != ZX_OK) {
      return zx::error(status);
    }
  }  // for (;;)
}
