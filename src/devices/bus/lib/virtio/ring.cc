// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <assert.h>
#include <inttypes.h>
#include <lib/stdcompat/bit.h>
#include <lib/virtio/device.h>
#include <lib/virtio/ring.h>
#include <limits.h>
#include <stdint.h>
#include <string.h>
#include <trace.h>

#include <fbl/algorithm.h>

#include <ktl/enforce.h>

#define LOCAL_TRACE 0

namespace virtio {

void virtio_dump_desc(const struct vring_desc* desc) {
  printf("vring descriptor %p: ", desc);
  printf("[addr=%#" PRIx64 ", ", desc->addr);
  printf("len=%d, ", desc->len);
  printf("flags=%#04hx, ", desc->flags);
  printf("next=%#04hx]\n", desc->next);
}

Ring::Ring(Device* device) : device_(device) {}

Ring::Ring(Ring&& other) noexcept {
  index_ = other.index_;
  device_ = other.device_;
  other.device_ = nullptr;
  ring_buffer_ = other.ring_buffer_;
  ring_ = other.ring_;
  other.ring_ = vring{};
}

Ring::~Ring() {
  LTRACE_ENTRY_OBJ;

  // TODO (Herrera) : Implement kernel like dma buffer (src/devices/lib/dma-buffer/dma-buffer.cc)
    //zx_status_t status =
      //VmAspace::kernel_aspace()->FreeRegion(reinterpret_cast<vaddr_t>(ring_buffer_.ptr));
  //if (status != ZX_OK) {
    //LTRACEF("failed to free ring buffer: %d\n", status);
  //}
}

Ring& Ring::operator=(Ring&& other) noexcept {
  index_ = other.index_;
  device_ = other.device_;
  other.device_ = nullptr;
  ring_buffer_ = other.ring_buffer_;
  ring_ = other.ring_;
  other.ring_ = vring{};
  return *this;
}

zx_status_t Ring::Init(uint16_t index) {
  return Init(/*index=*/index, /*count=*/device_->GetRingSize(index));
}

zx_status_t Ring::Init(uint16_t index, uint16_t count) {
  LTRACEF("index %u, count %u\n", index, count);

  // check that count is a power of 2
  if (!ktl::has_single_bit(count)) {
    dprintf(CRITICAL, "ring count: %u is not a power of 2\n", count);
    return ZX_ERR_INVALID_ARGS;
  }

  index_ = index;

  // make sure the count is available in this ring
  uint16_t max_ring_size = device_->GetRingSize(index);
  if (count > max_ring_size) {
    dprintf(CRITICAL, "ring init count too big for hardware %u > %u\n", count, max_ring_size);
    return ZX_ERR_OUT_OF_RANGE;
  }

  // allocate a ring
  const size_t vring_required_size = vring_size(count, PAGE_SIZE);

  // DMA buffer size must be multiples of page size. Round up to the nearest
  // page size.
  const size_t dma_buffer_size = fbl::round_up(vring_required_size, static_cast<size_t>(PAGE_SIZE));
  LTRACEF("need %zu bytes\n", dma_buffer_size);

  zx_status_t status = VmAspace::kernel_aspace()->AllocContiguous(
      "vring", dma_buffer_size, &ring_buffer_.ptr, 0, VmAspace::VMM_FLAG_COMMIT,
      ARCH_MMU_FLAG_PERM_READ | ARCH_MMU_FLAG_PERM_WRITE);
  if (status != ZX_OK) {
    dprintf(CRITICAL, "failed to allocate ring buffer of size %zu: %d\n", dma_buffer_size, status);
    return status;
  }

  ring_buffer_.pa = vaddr_to_paddr(ring_buffer_.ptr);

  LTRACEF("allocated vring at %p, physical address %#" PRIxPTR "\n", ring_buffer_.ptr,
          ring_buffer_.pa);

  /* initialize the ring */
  vring_init(&ring_, count, ring_buffer_.ptr, PAGE_SIZE);

  ring_.free_list = 0xffff;
  ring_.free_count = 0;

  /* add all the descriptors to the free list */
  for (uint16_t i = 0; i < count; i++) {
    FreeDesc(i);
  }

  /* register the ring with the device */
  zx_paddr_t pa_desc = ring_buffer_.pa;
  zx_paddr_t pa_avail = ring_buffer_.pa + ((uintptr_t)ring_.avail - (uintptr_t)ring_.desc);
  zx_paddr_t pa_used = ring_buffer_.pa + ((uintptr_t)ring_.used - (uintptr_t)ring_.desc);
  device_->SetRing(index_, count, pa_desc, pa_avail, pa_used);

  return ZX_OK;
}

void Ring::FreeDesc(uint16_t desc_index) {
  LTRACEF("index %u free_count %u\n", desc_index, ring_.free_count);
  ring_.desc[desc_index].next = ring_.free_list;
  ring_.free_list = desc_index;
  ring_.free_count++;
}

struct vring_desc* Ring::AllocDescChain(uint16_t count, uint16_t* start_index) {
  if (ring_.free_count < count)
    return nullptr;

  /* start popping entries off the chain */
  struct vring_desc* last = 0;
  uint16_t last_index = 0;
  while (count > 0) {
    uint16_t i = ring_.free_list;
    assert(i < ring_.num);

    struct vring_desc* desc = &ring_.desc[i];

    ring_.free_list = desc->next;
    ring_.free_count--;

    if (last) {
      desc->flags |= VRING_DESC_F_NEXT;
      desc->next = last_index;
    } else {
      // first one
      desc->flags &= static_cast<uint16_t>(~VRING_DESC_F_NEXT);
      desc->next = 0;
    }
    last = desc;
    last_index = i;
    count--;
  }

  if (start_index) {
    *start_index = last_index;
  }

  return last;
}

void Ring::SubmitChain(uint16_t desc_index) {
  LTRACEF("desc %u\n", desc_index);

  /* add the chain to the available list */
  struct vring_avail* avail = ring_.avail;

  avail->ring[avail->idx & ring_.num_mask] = desc_index;
  // Write memory barrier before updating avail->idx; updates to the descriptor ring must be
  // visible before an updated avail->idx.
  hw_wmb();
  avail->idx++;
}

void Ring::Kick() {
  LTRACEF("entry\n");
  // Write memory barrier before notifying the device. Updates to avail->idx must be visible
  // before the device sees the wakeup notification (so it processes the latest descriptors).
  hw_mb();

  device_->RingKick(index_);
}

}  // namespace virtio
