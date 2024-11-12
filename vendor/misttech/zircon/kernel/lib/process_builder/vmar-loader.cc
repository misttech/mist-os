// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/process_builder/vmar-loader.h"

#include <lib/stdcompat/bit.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/vmar.h>
#include <zircon/assert.h>
#include <zircon/status.h>

namespace {

constexpr std::string_view kVmoNameUnknown = "<unknown ELF file>";

constexpr std::string_view kVmoNamePrefixData = "data";
constexpr std::string_view kVmoNamePrefixBss = "bss";

constexpr char kHexDigits[] = "0132456789ABCDEF";

template <const ktl::string_view& Prefix>
void SetVmoName(fbl::RefPtr<VmObject> vmo, ktl::string_view base_name, size_t n) {
  ktl::array<char, ZX_MAX_NAME_LEN> buffer{};
  cpp20::span vmo_name(buffer);

  // First, "data" or "bss".
  size_t name_idx = Prefix.copy(vmo_name.data(), vmo_name.size());

  // Then the ordinal in hex (almost surely just one digit, but who knows).
  // Count the bits and divide with round-up to count the nybbles.
  const size_t hex_chars = (cpp20::bit_width(n | 1) + 3) / 4;
  cpp20::span hex = cpp20::span(vmo_name).subspan(name_idx, hex_chars);
  for (auto it = hex.rbegin(); it != hex.rend(); ++it) {
    *it = kHexDigits[n & 0xf];
    n >>= 4;
  }
  name_idx += hex.size();

  // Then `:`, it's guaranteed that the worst case "dataffffffffffffffff:" (21)
  // definitely fits in ZX_MAX_NAME_LEN (32).
  vmo_name[name_idx++] = ':';

  // Finally append the original VMO name, however much fits.
  cpp20::span avail = vmo_name.subspan(name_idx);
  name_idx += base_name.copy(avail.data(), avail.size());
  ZX_DEBUG_ASSERT(name_idx <= vmo_name.size());

  // memory->set_zx_name(vmo_name.data());
}

}  // namespace

namespace process_builder {

VmarLoader::VmoName VmarLoader::GetVmoName(fbl::RefPtr<VmObject> vmo) {
  VmarLoader::VmoName base_vmo_name{};

  if (base_vmo_name.front() == '\0') {
    kVmoNameUnknown.copy(base_vmo_name.data(), base_vmo_name.size());
  }
  return base_vmo_name;
}

zx_status_t VmarLoader::AllocateVmar(size_t vaddr_size, size_t vaddr_start,
                                     std::optional<size_t> vmar_offset) {
  ZX_DEBUG_ASSERT_MSG(!load_image_vmar_, "AllocateVmar called twice");
  zx_vaddr_t child_addr = 0;
  // constexpr zx_vm_option_t kFlags =
  //     ZX_VM_CAN_MAP_READ | ZX_VM_CAN_MAP_WRITE | ZX_VM_CAN_MAP_EXECUTE | ZX_VM_CAN_MAP_SPECIFIC;
  zx_status_t status = ZX_OK;
  /*vmar_->Allocate(kFlags | (vmar_offset ? ZX_VM_SPECIFIC : 0), vmar_offset.value_or(0),
                  vaddr_size, &load_image_vmar_, &child_addr);*/

  if (status == ZX_OK) {
    // Convert the absolute address of the child VMAR to the load bias relative
    // to the link-time vaddr.
    load_bias_ = child_addr - vaddr_start;
  }

  return status;
}

// This has both explicit instantiations below.
template <bool ZeroInVmo>
zx_status_t VmarLoader::MapWritable(uintptr_t vmar_offset, fbl::RefPtr<VmObject> vmo, bool copy_vmo,
                                    ktl::string_view base_name, uint64_t vmo_offset, size_t size,
                                    size_t& num_data_segments) {
  if constexpr (!ZeroInVmo) {
    ZX_DEBUG_ASSERT((size & (page_size() - 1)) == 0);
  }

  fbl::RefPtr<VmObject> writable_vmo;
  fbl::RefPtr<VmObject> map_vmo;
  if (copy_vmo) {
    /*zx_status_t status = ZX_OK;

        vmo->create_child(ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE, vmo_offset, size, &writable_vmo);
    if (status != ZX_OK) [[unlikely]] {
      return status;
    }*/
    // map_vmo = writable_vmo.borrow();
  } else {
    // map_vmo = vmo->borrow();
  }

  // If the size is not page-aligned, zero the last page beyond the size.
  if constexpr (ZeroInVmo) {
    const size_t subpage_size = size & (page_size() - 1);
    if (subpage_size > 0) {
      // const size_t zero_offset = size;
      // const size_t zero_size = page_size() - subpage_size;
      //  zx_status_t status = map_vmo->op_range(ZX_VMO_OP_ZERO, zero_offset, zero_size, nullptr,
      //  0); if (status != ZX_OK) [[unlikely]] {
      //    return status;
      //  }
    }
  }

  SetVmoName<kVmoNamePrefixData>(map_vmo, base_name, num_data_segments++);

  return Map(vmar_offset, kMapWritable, map_vmo, 0, size);
}

// Explicitly instantiate both flavors.

template zx_status_t VmarLoader::MapWritable<false>(  //
    uintptr_t vmar_offset, fbl::RefPtr<VmObject> memory, bool copy_vmo, ktl::string_view base_name,
    uint64_t vmo_offset, size_t size, size_t& num_data_segments);

template zx_status_t VmarLoader::MapWritable<true>(  //
    uintptr_t vmar_offset, fbl::RefPtr<VmObject> memory, bool copy_vmo, ktl::string_view base_name,
    uint64_t vmo_offset, size_t size, size_t& num_data_segments);

zx_status_t VmarLoader::MapZeroFill(uintptr_t vmar_offset, ktl::string_view base_name, size_t size,
                                    size_t& num_zero_segments) {
  fbl::RefPtr<VmObject> vmo;

  /*zx_status_t status = zx::vmo::create(size, 0, &vmo);
  if (status != ZX_OK) [[unlikely]] {
    return status;
  }*/

  SetVmoName<kVmoNamePrefixBss>(vmo, base_name, num_zero_segments++);

  return Map(vmar_offset, kMapWritable, vmo, 0, size);
}

}  // namespace process_builder
