// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/paging.h>

#include <fbl/algorithm.h>
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
constexpr uintptr_t PageAlignDown(uintptr_t p) { return fbl::round_down(p, ZX_PAGE_SIZE); }
constexpr uintptr_t PageAlignUp(uintptr_t p) { return fbl::round_up(p, ZX_PAGE_SIZE); }

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

HandoffPrep::VirtualAddressAllocator
HandoffPrep::VirtualAddressAllocator::PermanentHandoffDataAllocator(const ElfImage& kernel) {
  return {
      /*start=*/kernel.load_address() - ZX_PAGE_SIZE,
      /*strategy=*/HandoffPrep::VirtualAddressAllocator::Strategy::kDown,
      /*boundary=*/kernel.load_address() - k1GiB,
  };
}

HandoffPrep::VirtualAddressAllocator
HandoffPrep::VirtualAddressAllocator::FirstClassMappingAllocator(const ElfImage& kernel) {
  return {
      /*start=*/kernel.load_address() + kernel.aligned_memory_image().size_bytes() + ZX_PAGE_SIZE,
      /*strategy=*/HandoffPrep::VirtualAddressAllocator::Strategy::kUp,
      /*boundary=*/ktl::nullopt,
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

    case Strategy::kUp:
      if (boundary_) {
        ZX_DEBUG_ASSERT(size <= *boundary_);
        ZX_DEBUG_ASSERT(start_ <= *boundary_ - size);
      }
      return ktl::exchange(start_, start_ + size);
  }
  __UNREACHABLE;
}

void* HandoffPrep::CreateMapping(const PhysMapping& mapping) {
  // TODO(https://fxbug.dev/42164859): Debug assert that mapping.vaddr is >= to
  // the base of the high kernel address space.
  AddressSpace::MapSettings settings;
  switch (mapping.type) {
    case PhysMapping::Type::kNormal: {
      arch::AccessPermissions access = ToAccessPermissions(mapping.perms);
      settings = AddressSpace::NormalMapSettings(access);
      break;
    }
    case PhysMapping::Type::kMmio:
      ZX_DEBUG_ASSERT(mapping.perms.readable());
      ZX_DEBUG_ASSERT(mapping.perms.writable());
      ZX_DEBUG_ASSERT(!mapping.perms.executable());
      settings = AddressSpace::MmioMapSettings();
      break;
  }
  AddressSpace::PanicIfError(
      gAddressSpace->Map(mapping.vaddr, mapping.size, mapping.paddr, settings));

  return reinterpret_cast<void*>(mapping.vaddr);
}

void* HandoffPrep::PublishSingleMappingVmar(PhysMapping mapping) {
  PhysVmarPrep prep =
      PrepareVmarAt(ktl::string_view{mapping.name.data()}, mapping.vaddr, mapping.size);
  void* addr = prep.PublishMapping(ktl::move(mapping));
  ktl::move(prep).Publish();
  return addr;
}

void HandoffPrep::ConstructKernelAddressSpace(const UartDriver& uart) {
  // Physmap.
  {  // Shadowing the entire physmap would be redundantly wasteful.
    PhysMapping mapping("physmap"sv, PhysMapping::Type::kNormal, kArchPhysmapVirtualBase,
                        kArchPhysmapSize, 0, PhysMapping::Permissions::Rw(),
                        /*kasan_shadow=*/false);
    PublishSingleMappingVmar(ktl::move(mapping));
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

      PhysMapping mapping({}, PhysMapping::Type::kNormal, vaddr, segment.memsz(), paddr, perms);
      snprintf(mapping.name.data(), mapping.name.size(), "segment (p_vaddr = %#zx)",
               segment.vaddr());
      prep.PublishMapping(ktl::move(mapping));
      return true;
    });
    ktl::move(prep).Publish();
  }

  // UART.
  uart.Visit([&]<typename KernelDriver>(const KernelDriver& driver) {
    if constexpr (uart::MmioDriver<typename KernelDriver::uart_type>) {
      uart::MmioRange mmio = driver.mmio_range();
      uint64_t aligned_paddr = PageAlignDown(mmio.address);
      uint64_t aligned_size = PageAlignUp(mmio.size + (mmio.address - aligned_paddr));
      PhysMapping mapping("UART"sv, PhysMapping::Type::kMmio,
                          first_class_mapping_allocator_.AllocatePages(aligned_size), aligned_size,
                          aligned_paddr, PhysMapping::Permissions::Rw());
      void* aligned_vaddr = PublishSingleMappingVmar(ktl::move(mapping));
      handoff_->uart_mmio.base = reinterpret_cast<volatile void*>(
          reinterpret_cast<uintptr_t>(aligned_vaddr) + (mmio.address - aligned_paddr));
      handoff_->uart_mmio.size = mmio.size;
    }
  });
}
