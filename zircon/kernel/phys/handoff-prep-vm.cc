// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/paging.h>

#include <ktl/string_view.h>
#include <phys/address-space.h>
#include <phys/elf-image.h>

#include "handoff-prep.h"

#include <ktl/enforce.h>

//
// At a high-level, the kernel virtual address space is constructed as
// follows (taking the kernel's load address as an input):
//
// * The physmap is a fixed mapping below the rest of the others
// * Virtual addresses for temporary and permanent hand-off data are
//   bump-allocated downward (in 1GiB-separated ranges) below the kernel's
//   memory image (wherever it was loaded)
// * The remaining, various first-class mappings are made just above the
//   kernel's memory image (wherever it was loaded), with virtual address
//   ranges bump-allocated upward.
//
//            ...
//     other first-class     (↑)
//          mappings
// -------------------------
//       hole (1 page)
// -------------------------
//    kernel memory image
// -------------------------
//       hole (1 page)
// -------------------------
//  permanent hand-off data  (↓)
// ------------------------- kernel load address - 1GiB
//  temporary hand-off data  (↓)
//            ...
// ------------------------- kArchPhysmapVirtualBase
//          physmap
// ------------------------- kArchPhysmapVirtualBase + kArchPhysmapSize
//

namespace {

constexpr size_t k1GiB = 0x4000'0000;

constexpr bool IsPageAligned(uintptr_t p) { return p % ZX_PAGE_SIZE == 0; }

constexpr arch::AccessPermissions ToAccessPermissions(PhysMapping::Permissions perms) {
  return {
      .readable = perms.readable(),
      .writable = perms.writable(),
      .executable = perms.executable(),
  };
}

}  // namespace

HandoffPrep::VirtualAddressAllocator
HandoffPrep::VirtualAddressAllocator::TemporaryHandoffDataAllocator(const ElfImage& kernel) {
  return {
      /*start=*/kernel.load_address() - k1GiB,
      /*strategy=*/HandoffPrep::VirtualAddressAllocator::Strategy::kDown,
      /*boundary=*/kArchPhysmapVirtualBase + kArchPhysmapSize,
  };
}

uintptr_t HandoffPrep::VirtualAddressAllocator::AllocatePages(size_t size) {
  ZX_DEBUG_ASSERT(IsPageAligned(size));
  switch (strategy_) {
    case Strategy::kDown:
      ZX_DEBUG_ASSERT(start_ >= size);
      if (boundary_) {
        ZX_DEBUG_ASSERT(start_ - size >= *boundary_);
      }
      start_ -= size;
      return start_;
  }
  __UNREACHABLE;
}

void* HandoffPrep::CreateMapping(const PhysMapping& mapping, MappingType type) {
  // TODO(https://fxbug.dev/42164859): Debug assert that mapping.vaddr is >= to
  // the base of the high kernel address space.
  AddressSpace::MapSettings settings;
  switch (type) {
    case MappingType::kNormal: {
      arch::AccessPermissions access = ToAccessPermissions(mapping.perms);
      settings = AddressSpace::NormalMapSettings(access);
    }
  }
  AddressSpace::PanicIfError(
      gAddressSpace->Map(mapping.vaddr, mapping.size, mapping.paddr, settings));

  return reinterpret_cast<void*>(mapping.vaddr);
}

void HandoffPrep::PublishSingleMappingVmar(PhysMapping mapping, MappingType type) {
  PhysVmarPrep prep =
      PrepareVmarAt(ktl::string_view{mapping.name.data()}, mapping.vaddr, mapping.size);
  prep.PublishMapping(ktl::move(mapping), type);
  ktl::move(prep).Publish();
}

void HandoffPrep::ConstructKernelAddressSpace() {
  // Physmap.
  {
    // Shadowing the entire physmap would be redundantly wasteful.
    PhysMapping mapping("physmap"sv, kArchPhysmapVirtualBase, kArchPhysmapSize, 0,
                        PhysMapping::Permissions::Rw(), /*kasan_shadow=*/false);
    PublishSingleMappingVmar(ktl::move(mapping), MappingType::kNormal);
  }

  // The kernel's mapping.
  {
    PhysVmarPrep prep = PrepareVmarAt("kernel"sv, kernel_.load_address(), kernel_.vaddr_size());
    kernel_.load_info().VisitSegments([this, &prep](const auto& segment) {
      uintptr_t vaddr = segment.vaddr() + kernel_.load_bias();
      uintptr_t paddr = kernel_.physical_load_address() + segment.offset();

      PhysMapping::Permissions perms = PhysMapping::Permissions::FromSegment(segment);
      // If the segment is executable and the hardware doesn't support
      // executable-only mappings, we fix up the permissions as also readable.
      if constexpr (!AddressSpace::kExecuteOnlyAllowed) {
        if (segment.executable()) {
          perms.set_readable();
        }
      }

      PhysMapping mapping({}, vaddr, segment.memsz(), paddr, perms);
      snprintf(mapping.name.data(), mapping.name.size(), "segment (p_vaddr = %#zx)",
               segment.vaddr());
      prep.PublishMapping(ktl::move(mapping), MappingType::kNormal);
      return true;
    });
    ktl::move(prep).Publish();
  }
}
