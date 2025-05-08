// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <align.h>
#include <inttypes.h>
#include <lib/userabi/rodso.h>

#include <ktl/array.h>
#include <object/handle.h>
#include <object/vm_address_region_dispatcher.h>
#include <object/vm_object_dispatcher.h>
#include <vm/vm_address_region.h>
#include <vm/vm_aspace.h>
#include <vm/vm_object.h>

#include <ktl/enforce.h>

// Map one segment from our VM object.
zx_status_t RoDso::MapSegment(fbl::RefPtr<VmAddressRegionDispatcher> vmar, bool code,
                              size_t vmar_offset, size_t start_offset, size_t end_offset) const {
  uint32_t flags = ZX_VM_SPECIFIC | ZX_VM_PERM_READ;
  if (code)
    flags |= ZX_VM_PERM_EXECUTE;

  size_t len = end_offset - start_offset;

  zx::result<VmAddressRegion::MapResult> mapping_result =
      vmar->Map(vmar_offset, vmo_, start_offset, len, flags);

  ktl::array<char, ZX_MAX_NAME_LEN> name;
  vmo_->get_name(name.data(), name.size());
  const char* segment_name = code ? "code" : "rodata";
  if (mapping_result.is_error()) {
    dprintf(CRITICAL, "userboot: %s %s mapping %#zx @ %#" PRIxPTR " size %#zx failed %d\n",
            name.data(), segment_name, start_offset, vmar->vmar()->base() + vmar_offset, len,
            mapping_result.status_value());
  } else {
    DEBUG_ASSERT(mapping_result->base == vmar->vmar()->base() + vmar_offset);
    dprintf(SPEW, "userboot: %-12s ELF %-6s %#7zx @ [%#" PRIxPTR ",%#" PRIxPTR ")\n", name.data(),
            segment_name, start_offset, mapping_result->base, mapping_result->base + len);
  }

  return mapping_result.status_value();
}

zx_status_t RoDso::Map(fbl::RefPtr<VmAddressRegionDispatcher> vmar, size_t offset) const {
  zx_status_t status = MapSegment(vmar, false, offset, 0, code_start_);
  if (status == ZX_OK)
    status = MapSegment(ktl::move(vmar), true, offset + code_start_, code_start_, size_);
  return status;
}
