// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/paging.h>
#include <lib/elfldltl/machine.h>
#include <lib/memalloc/range.h>

#include <fbl/algorithm.h>
#include <ktl/bit.h>
#include <ktl/iterator.h>
#include <ktl/string_view.h>
#include <phys/address-space.h>
#include <phys/allocation.h>
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

uint64_t PhysmapSize() {
  // Find the highest RAM address, which gives the size of the physmap given
  // the nature of the mapping.
  auto last_ram = ktl::prev(Allocation::GetPool().end());
  while (!memalloc::IsRamType(last_ram->type)) {  // There can't not be any RAM
    --last_ram;
  }
  return PageAlignUp(last_ram->end());
}

}  // namespace

HandoffPrep::VirtualAddressAllocator
HandoffPrep::VirtualAddressAllocator::TemporaryHandoffDataAllocator(const ElfImage& kernel) {
  return {
      /*start=*/kernel.load_address() - k1GiB,
      /*strategy=*/HandoffPrep::VirtualAddressAllocator::Strategy::kDown,
      /*boundary=*/kArchPhysmapVirtualBase + PhysmapSize(),
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

MappedMemoryRange HandoffPrep::CreateMapping(const PhysMapping& mapping) {
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

  return {{reinterpret_cast<ktl::byte*>(mapping.vaddr), mapping.size}, mapping.paddr};
}

MappedMemoryRange HandoffPrep::PublishSingleMappingVmar(PhysMapping mapping) {
  PhysVmarPrep prep =
      PrepareVmarAt(ktl::string_view{mapping.name.data()}, mapping.vaddr, mapping.size);
  MappedMemoryRange mapped = prep.PublishMapping(ktl::move(mapping));
  ktl::move(prep).Publish();
  return mapped;
}

MappedMemoryRange HandoffPrep::PublishSingleMappingVmar(ktl::string_view name,
                                                        PhysMapping::Type type, uintptr_t addr,
                                                        size_t size,
                                                        PhysMapping::Permissions perms) {
  uint64_t aligned_paddr = PageAlignDown(addr);
  uint64_t aligned_size = PageAlignUp(size + (addr - aligned_paddr));

  // TODO(https://fxbug.dev/379891035): Revisit if kasan_shadow = true is the
  // right default for the mappings created with this utility.
  PhysMapping mapping{
      name,                                                        //
      type,                                                        //
      first_class_mapping_allocator_.AllocatePages(aligned_size),  //
      aligned_size,                                                //
      aligned_paddr,                                               //
      perms,                                                       //
  };
  ktl::span aligned = PublishSingleMappingVmar(ktl::move(mapping));
  return {aligned.subspan(addr - aligned_paddr, size), addr};
}

MappedMemoryRange HandoffPrep::PublishStackVmar(ZirconAbiSpec::Stack stack, memalloc::Type type) {
  ktl::string_view name = memalloc::ToString(type);

  ZX_DEBUG_ASSERT_MSG(IsPageAligned(stack.size_bytes), "%.*s size (%#x) is not page-aligned",
                      static_cast<int>(name.size()), name.data(), stack.size_bytes);
  ZX_DEBUG_ASSERT_MSG(IsPageAligned(stack.lower_guard_size_bytes),
                      "%.*s lower guard size (%#x) is not page-aligned",
                      static_cast<int>(name.size()), name.data(), stack.size_bytes);
  ZX_DEBUG_ASSERT_MSG(IsPageAligned(stack.upper_guard_size_bytes),
                      "%.*s upper guard size (%#x) is not page-aligned",
                      static_cast<int>(name.size()), name.data(), stack.size_bytes);

  size_t vmar_size = stack.lower_guard_size_bytes + stack.size_bytes + stack.upper_guard_size_bytes;
  uintptr_t base = first_class_mapping_allocator_.AllocatePages(vmar_size);
  auto prep = PrepareVmarAt(name, base, vmar_size);
  uint64_t paddr = Allocation::GetPool().Allocate(type, stack.size_bytes, ZX_PAGE_SIZE).value();
  PhysMapping mapping{
      name,                                 //
      PhysMapping::Type::kNormal,           //
      base + stack.lower_guard_size_bytes,  //
      stack.size_bytes,                     //
      paddr,                                //
      PhysMapping::Permissions::Rw(),       //
  };
  MappedMemoryRange mapped = prep.PublishMapping(mapping);
  memset(mapped.data(), 0, mapped.size_bytes());
  ktl::move(prep).Publish();
  return mapped;
}

HandoffPrep::ZirconAbi HandoffPrep::ConstructKernelAddressSpace(const UartDriver& uart) {
  const memalloc::Pool& pool = Allocation::GetPool();
  ZirconAbi abi{};

  // Physmap.
  {
    PhysVmarPrep prep = PrepareVmarAt("physmap"sv, kArchPhysmapVirtualBase, PhysmapSize());
    auto map = [&prep](const memalloc::Range& range) {
      uint64_t aligned_paddr = PageAlignDown(range.addr);
      uint64_t aligned_size = PageAlignUp(range.size + (range.addr - aligned_paddr));

      // Shadowing the physmap would be redundantly wasteful.
      PhysMapping mapping("RAM"sv, PhysMapping::Type::kNormal,
                          kArchPhysmapVirtualBase + aligned_paddr, aligned_size, aligned_paddr,
                          PhysMapping::Permissions::Rw(),
                          /*kasan_shadow=*/false);
      prep.PublishMapping(mapping);
      return true;
    };
    memalloc::NormalizeRam(pool, map);
    ktl::move(prep).Publish();
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

  // Kernel ABI
  {
    ktl::span machine_stack =
        PublishStackVmar(abi_spec_.machine_stack, memalloc::Type::kBootMachineStack);
    abi.machine_stack_top = elfldltl::AbiTraits<>::InitialStackPointer(
        reinterpret_cast<uintptr_t>(machine_stack.data()), machine_stack.size_bytes());

    if (abi_spec_.shadow_call_stack.size_bytes > 0) {
      ktl::span shadow_call_stack =
          PublishStackVmar(abi_spec_.shadow_call_stack, memalloc::Type::kBootShadowCallStack);
      abi.shadow_call_stack_base = reinterpret_cast<uintptr_t>(shadow_call_stack.data());
    }
  }

  // Periphmap
  {
    auto periph_filter = [](memalloc::Type type) -> ktl::optional<memalloc::Type> {
      return type == memalloc::Type::kPeripheral ? ktl::make_optional(type) : ktl::nullopt;
    };

    // Count the number of peripheral ranges...
    size_t count = 0;
    {
      auto count_ranges = [&count](const memalloc::Range& range) {
        ZX_DEBUG_ASSERT(range.type == memalloc::Type::kPeripheral);
        ++count;
        return true;
      };
      memalloc::NormalizeRanges(pool, count_ranges, periph_filter);
    }

    // ...so that we can allocate the number of such mappings in the hand-off.
    fbl::AllocChecker ac;
    ktl::span periph_ranges = New(handoff_->periph_ranges, ac, count);
    ZX_ASSERT(ac.check());

    auto map = [this, &periph_ranges](const memalloc::Range& range) {
      ZX_DEBUG_ASSERT(range.type == memalloc::Type::kPeripheral);
      periph_ranges.front() = PublishSingleMmioMappingVmar("periphmap"sv, range.addr, range.size);
      periph_ranges = periph_ranges.last(periph_ranges.size() - 1);
      return true;
    };
    memalloc::NormalizeRanges(pool, map, periph_filter);
  }

  // UART.
  uart.Visit([&]<typename KernelDriver>(const KernelDriver& driver) {
    if constexpr (uart::MmioDriver<typename KernelDriver::uart_type>) {
      uart::MmioRange mmio = driver.mmio_range();
      handoff_->uart_mmio = PublishSingleMmioMappingVmar("UART"sv, mmio.address, mmio.size);
    }
  });

  // NVRAM
  {
    auto nvram = ktl::find_if(pool.begin(), pool.end(), [](const memalloc::Range& range) {
      return range.type == memalloc::Type::kNvram;
    });
    if (nvram != pool.end()) {
      handoff_->nvram = PublishSingleWritableDataMappingVmar("NVRAM"sv, nvram->addr, nvram->size);
    }
  }

  // Construct the arch-specific bits at the end (to give the non-arch-specific
  // placements in the address space a small amount of relative familiarity).
  ArchConstructKernelAddressSpace();

  return abi;
}
