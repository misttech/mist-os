// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/elfldltl/vmar-loader.h"

#include <lib/zx/vmar.h>
#include <zircon/assert.h>
#include <zircon/status.h>

namespace {
constexpr std::string_view kVmoNameUnknown = "<unknown ELF file>";
}

namespace elfldltl {

VmarLoader::VmoName VmarLoader::GetVmoName(zx::unowned_vmo vmo) {
  VmarLoader::VmoName base_vmo_name{};
  zx_status_t status = vmo->get_property(ZX_PROP_NAME, base_vmo_name.data(), base_vmo_name.size());

  if (status != ZX_OK || base_vmo_name[0] == '\0') {
    memcpy(base_vmo_name.data(), kVmoNameUnknown.data(), kVmoNameUnknown.size());
  }

  return base_vmo_name;
}

template <const std::string_view& Prefix>
void VmarLoader::SetVmoName(zx::unowned_vmo vmo, std::string_view base_name, size_t n) {
  VmoName vmo_name{};
  memcpy(vmo_name.data(), Prefix.data(), Prefix.size());

  auto hex_chars = (cpp20::bit_width(n | 1) + 3) / 4;
  constexpr char alphabet[] = "0132456789ABCDEF";

  size_t hex_mask = n;
  size_t index = hex_chars + Prefix.size() - 1;
  while (hex_mask != 0) {
    vmo_name[index] = alphabet[hex_mask & 0x0F];
    hex_mask >>= 4;
    index--;
  }
  vmo_name[Prefix.size() + hex_chars] = ':';

  memcpy(vmo_name.data() + Prefix.size() + hex_chars + 1, base_name.data(),
         ZX_MAX_NAME_LEN - vmo_name.size());

  vmo->set_property(ZX_PROP_NAME, vmo_name.data(), vmo_name.size());
}

template void VmarLoader::SetVmoName<VmarLoader::kVmoNamePrefixData>(zx::unowned_vmo vmo,
                                                                     std::string_view base_name,
                                                                     size_t n);
template void VmarLoader::SetVmoName<VmarLoader::kVmoNamePrefixBss>(zx::unowned_vmo vmo,
                                                                    std::string_view base_name,
                                                                    size_t n);

zx_status_t VmarLoader::AllocateVmar(size_t vaddr_size, size_t vaddr_start) {
  zx_vaddr_t child_addr;
  zx_status_t status = vmar_->allocate(
      ZX_VM_CAN_MAP_READ | ZX_VM_CAN_MAP_WRITE | ZX_VM_CAN_MAP_EXECUTE | ZX_VM_CAN_MAP_SPECIFIC, 0,
      vaddr_size, &child_vmar_, &child_addr);

  if (status == ZX_OK) {
    // Convert the absolute address of the child VMAR to a platform-generic address type
    // needed for the generic DirectMemory abstraction.
    memory_.set_image({reinterpret_cast<std::byte*>(child_addr), vaddr_size});
    memory_.set_base(vaddr_start);
  }

  return status;
}

zx_status_t VmarLoader::MapAnonymousData(zx_vm_option_t options, size_t zero_size,
                                         std::string_view base_name, size_t vmar_offset,
                                         size_t& num_mapped_anonymous_regions) {
  // Create a zero-fill VMO for the entire remainder of the segment, including intersecting
  // page.
  zx::vmo zero_fill_vmo;
  zx_status_t status = zx::vmo::create(zero_size, 0, &zero_fill_vmo);

  if (status != ZX_OK) [[unlikely]] {
    return status;
  }

  SetVmoName<kVmoNamePrefixBss>(zero_fill_vmo.borrow(), base_name, num_mapped_anonymous_regions);
  num_mapped_anonymous_regions++;

  zx_vaddr_t mapping_addr;
  return child_vmar_.map(options, vmar_offset, zero_fill_vmo, 0, zero_size, &mapping_addr);
}

zx_status_t VmarLoader::MapFileData(size_t segment_offset, bool is_writable, zx_vm_option_t options,
                                    size_t map_size, zx::unowned_vmo vmo,
                                    std::string_view base_name, size_t vmar_offset,
                                    size_t& num_mapped_file_regions) {
  zx::vmo cloned_vmo;

  // If we don't create a new child, we map in from the segment offset. If we do create
  // a new child, we map in from the start of the child. mapping_vmo_offset captures
  // the conditional offset into the mapping VMO.
  size_t mapping_vmo_offset = segment_offset;

  if (is_writable) {
    zx_status_t status = vmo->create_child(ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE, segment_offset,
                                           map_size, &cloned_vmo);

    if (status != ZX_OK) [[unlikely]] {
      return status;
    }

    // The mapping_vmo contains exclusively the data we want to map, so our mapping
    // offset is 0.
    mapping_vmo_offset = 0;

    SetVmoName<kVmoNamePrefixData>(cloned_vmo.borrow(), base_name, num_mapped_file_regions);
    num_mapped_file_regions++;
  }

  zx_vaddr_t mapping_addr;
  return child_vmar_.map(options, vmar_offset, cloned_vmo ? *(cloned_vmo.borrow()) : *vmo,
                         mapping_vmo_offset, map_size, &mapping_addr);
}

}  // namespace elfldltl
