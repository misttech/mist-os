// Copyright (c) 2022 The Fuchsia Authors
//
// Permission to use, copy, modify, and/or distribute this software for any purpose with or without
// fee is hereby granted, provided that the above copyright notice and this permission notice
// appear in all copies.
//
// THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH REGARD TO THIS
// SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE
// AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
// WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT,
// NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE
// OF THIS SOFTWARE.

#define _ALL_SOURCE  // Define this so we can use thrd_create_with_name

#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/internal_mem_allocator.h"

#include <inttypes.h>
#include <lib/async/cpp/task.h>
#include <lib/zx/vmar.h>
#include <lib/zx/vmo.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>

#include <wlan/drivers/log.h>

#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/align.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/debug.h"

namespace wlan::nxpfmac {

using Region = RegionAllocator::Region;
constexpr size_t kRegionBookkeepingMemorySize = 2048;
constexpr uint32_t kVmoId = std::numeric_limits<int32_t>::max();

zx_status_t InternalMemAllocator::Create(BusInterface* bus, size_t vmo_size,
                                         std::unique_ptr<InternalMemAllocator>* out_allocator) {
  if (out_allocator == nullptr) {
    return ZX_ERR_INVALID_ARGS;
  }

  std::unique_ptr<InternalMemAllocator> mem_allocator(new InternalMemAllocator(bus));

  zx_status_t status = mem_allocator->CreateAndPrepareVmo(vmo_size);
  if (status != ZX_OK) {
    NXPF_ERR("Failed to create VMO: %s", zx_status_get_string(status));
    return status;
  }

  *out_allocator = std::move(mem_allocator);
  return ZX_OK;
}

InternalMemAllocator::InternalMemAllocator(BusInterface* bus)
    : bus_(bus), allocator_(RegionAllocator::RegionPool::Create(kRegionBookkeepingMemorySize)) {}

InternalMemAllocator::~InternalMemAllocator() {
  zx_status_t status = bus_->ReleaseVmo(kVmoId);
  if (status != ZX_OK) {
    NXPF_ERR("Failed to unregister internal VMO from SDIO bus: %s", zx_status_get_string(status));
  }
  vmo_.reset();
}

zx::result<uint8_t*> InternalMemAllocator::MapInternalVmo(zx_handle_t vmo, uint64_t vmo_size) {
  zx_vaddr_t addr = 0;
  zx_status_t err = zx_vmar_map(zx_vmar_root_self(), ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo, 0,
                                vmo_size, &addr);
  if (err != ZX_OK) {
    NXPF_ERR("Unable to map VMO: %s", zx_status_get_string(err));
    return zx::error(err);
  }

  return zx::success(reinterpret_cast<uint8_t*>(addr));
}

zx_status_t InternalMemAllocator::CreateAndPrepareVmo(size_t vmo_size) {
  zx::vmo vmo;
  if (!vmo_size) {
    vmo_size = kDefaultInternalVmoSize;
  }
  zx_status_t ret = zx::vmo::create(vmo_size, 0, &vmo);
  if (ret != ZX_OK) {
    NXPF_ERR("Error creating internal VMO  %s", zx_status_get_string(ret));
    return ret;
  }

  zx::result<uint8_t*> address = MapInternalVmo(vmo.get(), vmo_size);
  if (address.is_error()) {
    NXPF_ERR("Failed to map internal VMO %s", address.status_string());
    return address.status_value();
  }

  ret = vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo_);
  if (ret != ZX_OK) {
    NXPF_ERR("Failed to duplicate internal VMO %s", zx_status_get_string(ret));
    return ret;
  }

  ret = bus_->PrepareVmo(kVmoId, std::move(vmo));
  if (ret != ZX_OK) {
    NXPF_ERR("Failed to prepare internal VMO: %s", zx_status_get_string(ret));
    return ret;
  }
  vmo_mapped_addr_ = address.value();

  ret = allocator_.AddRegion(
      {.base = reinterpret_cast<uint64_t>(vmo_mapped_addr_), .size = vmo_size});
  if (ret != ZX_OK) {
    NXPF_ERR("Failed to get VMO size: %s", zx_status_get_string(ret));
    return ret;
  }
  alloc_alignment_ = zx_system_get_dcache_line_size();
  vmo_size_ = vmo_size;
  NXPF_INFO("Data cache line size is: %d", alloc_alignment_);

  // Deliberately initializing memory to 0xa5 to catch uninitialized access.
  memset(vmo_mapped_addr_, 0xa5, vmo_size_);

  return ZX_OK;
}

void* InternalMemAllocator::Alloc(size_t size) {
  Region::UPtr region;
  zx_status_t status = allocator_.GetRegion(size + sizeof(HeadMetaData) + sizeof(TailMetaData),
                                            alloc_alignment_, region);
  if (status == ZX_OK) {
    uint64_t mem_ptr = region->base;
    uint64_t mem_size = region->size;
    // Set header meta data.
    auto head_md = reinterpret_cast<HeadMetaData*>(mem_ptr);
    memset(head_md, 0, sizeof(HeadMetaData));
    head_md->region = std::move(region);
    head_md->signature = kMetaDataSignature;
    // Set the tail meta data.
    auto tail_md = reinterpret_cast<TailMetaData*>(mem_ptr + mem_size - sizeof(TailMetaData));
    tail_md->signature = kMetaDataSignature;
    return reinterpret_cast<void*>(mem_ptr + sizeof(HeadMetaData));
  } else {
    num_alloc_fails_++;
    return nullptr;
  }
}

bool InternalMemAllocator::Free(void* mem_ptr) {
  if (!GetInternalVmoInfo(reinterpret_cast<uint8_t*>(mem_ptr))) {
    return false;
  }
  auto head_md =
      reinterpret_cast<HeadMetaData*>(reinterpret_cast<uint64_t>(mem_ptr) - sizeof(HeadMetaData));
  if (head_md->signature != kMetaDataSignature) {
    num_free_fails_++;
    // The header signature is corrupted. Cannot trust the region pointer.
    ZX_PANIC("Buffer: %p header signature: 0x%lx is corrupt", mem_ptr, head_md->signature);
    return true;
  }
  auto tail_md =
      reinterpret_cast<TailMetaData*>(reinterpret_cast<uint64_t>(mem_ptr) - sizeof(HeadMetaData) +
                                      head_md->region->size - sizeof(TailMetaData));
  if (tail_md->signature != kMetaDataSignature) {
    NXPF_ERR("Buffer: %p tail signature: 0x%llx is corrupt", mem_ptr, tail_md->signature);
    num_free_fails_++;
  }
  head_md->region.reset();
  return true;
}

bool InternalMemAllocator::GetInternalVmoInfo(uint8_t* buf_ptr, uint32_t* out_vmo_id,
                                              uint64_t* out_vmo_offset) {
  if ((buf_ptr >= vmo_mapped_addr_) && (buf_ptr < (vmo_mapped_addr_ + vmo_size_))) {
    if (out_vmo_id) {
      *out_vmo_id = kVmoId;
    }
    if (out_vmo_offset) {
      *out_vmo_offset = buf_ptr - vmo_mapped_addr_;
    }
    return true;
  }
  return false;
}

void InternalMemAllocator::LogStatus() {
  // Determine current total allocation size of all the regions.
  size_t alloc_size = 0;
  allocator_.WalkAllocatedRegions([&alloc_size](const ralloc_region_t* r) -> bool {
    alloc_size += r->size;
    return true;
  });
  NXPF_INFO(
      "Internal Mem Status: alloc reg: %lld avail reg: %lld alloc size: %u alloc fails: %lld free fails: %lld",
      allocator_.AllocatedRegionCount(), allocator_.AvailableRegionCount(), alloc_size,
      num_alloc_fails_, num_free_fails_);
}

}  // namespace wlan::nxpfmac
