// Copyright 2017 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT
//
#include <lib/console.h>
#include <lib/fit/defer.h>
#include <lib/smbios/smbios.h>
#include <stdint.h>
#include <string.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <ktl/utility.h>
#include <phys/handoff.h>
#include <platform/pc/smbios.h>
#include <vm/physmap.h>
#include <vm/vm_address_region.h>
#include <vm/vm_aspace.h>
#include <vm/vm_object_physical.h>

#include <ktl/enforce.h>

namespace {

static constexpr size_t kMaxEPSize =
    ktl::max(sizeof(smbios::EntryPoint2_1), sizeof(smbios::EntryPoint3_0));

smbios::EntryPointVersion kEpVersion = smbios::EntryPointVersion::Unknown;
union {
  const void* raw;
  const smbios::EntryPoint2_1* ep2_1;
} kEntryPoint;
uintptr_t kStructBase = 0;  // Address of first SMBIOS struct

zx::result<ktl::pair<fbl::RefPtr<VmMapping>, void*>> MapRange(uint64_t paddr, size_t len) {
  fbl::RefPtr<VmObjectPhysical> vmo;
  const uint64_t paddr_base = ROUNDDOWN_PAGE_SIZE(paddr);
  const uint64_t paddr_top = ROUNDUP_PAGE_SIZE(paddr + len);
  zx_status_t status = VmObjectPhysical::Create(paddr_base, paddr_top - paddr_base, &vmo);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  zx::result<VmAddressRegion::MapResult> mapping_result =
      VmAspace::kernel_aspace()->RootVmar()->CreateVmMapping(
          0, len, 0, 0 /* vmar_flags */, ktl::move(vmo), 0,
          ARCH_MMU_FLAG_CACHED | ARCH_MMU_FLAG_PERM_READ, "smbios");
  if (mapping_result.is_error()) {
    return mapping_result.take_error();
  }
  // Prepopulate the mapping's page tables so there are no page faults taken.
  status = mapping_result->mapping->MapRange(0, len, true);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  const uintptr_t vaddr = mapping_result->base + (paddr - paddr_base);
  ktl::pair<fbl::RefPtr<VmMapping>, void*> ret = {ktl::move(mapping_result->mapping),
                                                  reinterpret_cast<void*>(vaddr)};
  return zx::ok(ktl::move(ret));
}

zx_status_t FindEntryPoint(const void** base, smbios::EntryPointVersion* version) {
  // See if the ZBI told us where the table is.
  if (gPhysHandoff->smbios_phys) {
    uint64_t smbios = gPhysHandoff->smbios_phys.value();
    auto result = MapRange(smbios, kMaxEPSize);
    if (result.is_error()) {
      return result.status_value();
    }
    auto [mapping, p] = ktl::move(*result);
    if (!memcmp(p, SMBIOS2_ANCHOR, strlen(SMBIOS2_ANCHOR))) {
      *base = p;
      *version = smbios::EntryPointVersion::V2_1;
      return ZX_OK;
    } else if (!memcmp(p, SMBIOS3_ANCHOR, strlen(SMBIOS3_ANCHOR))) {
      *base = p;
      *version = smbios::EntryPointVersion::V3_0;
      return ZX_OK;
    }
    mapping->Destroy();
  }

  return ZX_ERR_NOT_FOUND;
}

zx_status_t MapStructs2_1(const smbios::EntryPoint2_1* ep, fbl::RefPtr<VmMapping>* mapping,
                          uintptr_t* struct_table_virt) {
  auto result = MapRange(ep->struct_table_phys, ep->struct_table_length);
  if (result.is_error()) {
    return result.status_value();
  }
  auto [map, base] = ktl::move(*result);
  *mapping = ktl::move(map);
  *struct_table_virt = reinterpret_cast<uintptr_t>(base);
  return ZX_OK;
}

}  // namespace

// Walk the known SMBIOS structures.  The callback will be called once for each
// structure found.
zx_status_t SmbiosWalkStructs(smbios::StructWalkCallback cb) {
  switch (kEpVersion) {
    case smbios::EntryPointVersion::V2_1: {
      return smbios::EntryPoint(kEntryPoint.ep2_1).WalkStructs(kStructBase, ktl::move(cb));
    }
    case smbios::EntryPointVersion::V3_0:
      return ZX_ERR_NOT_SUPPORTED;
    default:
      return ZX_ERR_NOT_SUPPORTED;
  }
}

void pc_init_smbios() {
  fbl::RefPtr<VmMapping> mapping;
  auto cleanup_mapping = fit::defer([&mapping] {
    if (mapping) {
      mapping->Destroy();
    }
  });

  const void* start = nullptr;
  auto version = smbios::EntryPointVersion::Unknown;
  uintptr_t struct_table_virt = 0;

  zx_status_t status = FindEntryPoint(&start, &version);
  if (status != ZX_OK) {
    printf("smbios: Failed to locate entry point\n");
    return;
  }

  switch (version) {
    case smbios::EntryPointVersion::V2_1: {
      auto ep = reinterpret_cast<const smbios::EntryPoint2_1*>(start);
      if (!ep->IsValid()) {
        return;
      }

      status = MapStructs2_1(ep, &mapping, &struct_table_virt);
      if (status != ZX_OK) {
        printf("smbios: failed to map structs: %d\n", status);
        return;
      }
      break;
    }
    case smbios::EntryPointVersion::V3_0:
      printf("smbios: version 3 not yet implemented\n");
      return;
    default:
      DEBUG_ASSERT(false);
      printf("smbios: Unknown version?\n");
      return;
  }

  kEntryPoint.raw = start;
  kEpVersion = version;
  kStructBase = struct_table_virt;
  cleanup_mapping.cancel();
}

static zx_status_t DebugStructWalk(smbios::SpecVersion ver, const smbios::Header* hdr,
                                   const smbios::StringTable& st) {
  switch (hdr->type) {
    case smbios::StructType::BiosInfo: {
      if (ver.IncludesVersion(2, 4)) {
        auto entry = reinterpret_cast<const smbios::BiosInformationStruct2_4*>(hdr);
        entry->Dump(st);
        return ZX_OK;
      } else if (ver.IncludesVersion(2, 0)) {
        auto entry = reinterpret_cast<const smbios::BiosInformationStruct2_0*>(hdr);
        entry->Dump(st);
        return ZX_OK;
      }
      break;
    }
    case smbios::StructType::SystemInfo: {
      if (ver.IncludesVersion(2, 4)) {
        auto entry = reinterpret_cast<const smbios::SystemInformationStruct2_4*>(hdr);
        entry->Dump(st);
        return ZX_OK;
      } else if (ver.IncludesVersion(2, 1)) {
        auto entry = reinterpret_cast<const smbios::SystemInformationStruct2_1*>(hdr);
        entry->Dump(st);
        return ZX_OK;
      } else if (ver.IncludesVersion(2, 0)) {
        auto entry = reinterpret_cast<const smbios::SystemInformationStruct2_0*>(hdr);
        entry->Dump(st);
        return ZX_OK;
      }
      break;
    }
    default:
      break;
  }
  printf("smbios: found struct@%p: typ=%u len=%u st_len=%zu\n", hdr,
         static_cast<uint8_t>(hdr->type), hdr->length, st.length());
  st.Dump();

  return ZX_OK;
}

static int CmdSmbios(int argc, const cmd_args* argv, uint32_t flags) {
  if (argc < 2) {
    printf("not enough arguments\n");
  usage:
    printf("usage:\n");
    printf("%s dump\n", argv[0].str);
    return ZX_ERR_INTERNAL;
  }

  if (!strcmp(argv[1].str, "dump")) {
    zx_status_t status = SmbiosWalkStructs(DebugStructWalk);
    if (status != ZX_OK) {
      printf("smbios: failed to walk structs: %d\n", status);
    }
    return ZX_OK;
  } else {
    printf("unknown command\n");
    goto usage;
  }

  return ZX_OK;
}

STATIC_COMMAND_START
STATIC_COMMAND("smbios", "smbios", &CmdSmbios)
STATIC_COMMAND_END(smbios)
