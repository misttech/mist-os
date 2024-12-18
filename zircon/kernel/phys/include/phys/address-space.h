// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_ADDRESS_SPACE_H_
#define ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_ADDRESS_SPACE_H_

#include <zircon/assert.h>

#include <hwreg/array.h>
#include <ktl/array.h>
#include <ktl/byte.h>
#include <ktl/functional.h>
#include <ktl/integer_sequence.h>
#include <ktl/move.h>
#include <ktl/optional.h>
#include <ktl/type_traits.h>
#include <phys/arch/address-space.h>

#include "allocation.h"

// Forward-declared; fully declared in <lib/memalloc/pool.h>.
namespace memalloc {
class Pool;
}  // namespace memalloc

// Ordered list from highest supported page size shift to lowest.
// When aligning ranges in memory, specially peripheral ranges, we attempt to
// align those ranges to these boundaries, in order, such that the generated
// mappings require a smaller set of pages.
constexpr ktl::array kAddressSpacePageSizeShifts = ktl::to_array<size_t>({
    // 1 GB
    12 + (9 * 2),
    // 2 MB
    12 + (9 * 1),
    // 4 KB
    12 + (9 * 0),
});

// Defined below.
class AddressSpace;

// The singleton address space expected to be used by a phys program,
// registered as any instance that calls AddressSpace::Install().
extern AddressSpace* gAddressSpace;

// Perform architecture-specific address space set-up.  This assumes that only
// the boot conditions hold and is expected to be called before "normal work"
// can proceed.  The AddressSpace object will be used as gAddressSpace and must
// not go out of scope.  It's usually on the stack of PhysMain.
void ArchSetUpAddressSpace(AddressSpace& aspace);

// Reset and repeat the work of ArchSetUpAddressSpace.  This reuses the
// AddressSpace object installed in gAddressSpace by ArchSetUpAddressSpace, but
// calls its Init() method to reset its state afresh.  This uses the current
// state of the Allocation pool for all new page table pages, so they will only
// be in physical pages that are currently free.  This is necessary to prepare
// for TrampolineBoot::Boot (after TrampolineBoot::Load has reserved whatever
// space it needs to from the pool).  Even the .bss space of the phys image
// itself may no longer be safe to use as page table pages, and pages allocated
// from the pool before TrampolineBoot::Load could overlap with memory that
// will be clobbered by the trampoline.
void ArchPrepareAddressSpaceForTrampoline();

// A representation of a virtual address space.
//
// This definition relies on two architecture-specific types being defined
// within <phys/arch/address-space.h>: ArchLowerPagingTraits and
// ArchUpperPagingTraits. These types are expected to be types meeting the
// <lib/arch/paging.h> "PagingTraits" API and give the descriptions of the
// upper and lower virtual address spaces. In the case of a unified address
// space spanning both the upper and lower, the single corresponding trait
// type is expected to be given as both ArchLowerPagingTraits and
// ArchUpperPagingTraits.
//
// Further, this type similarly relies on a function ArchCreatePagingState() to
// be defined in the header, creating the paging traits' coincidental
// SystemState specification. See Init() below.
//
// An AddressSpace must be manually installed (via Install()).
class AddressSpace {
 public:
  using LowerPaging = arch::Paging<ArchLowerPagingTraits>;
  using UpperPaging = arch::Paging<ArchUpperPagingTraits>;

  static_assert(ktl::is_same_v<typename LowerPaging::MemoryType, typename UpperPaging::MemoryType>);
  using MemoryType = typename LowerPaging::MemoryType;

  static_assert(
      ktl::is_same_v<typename LowerPaging::SystemState, typename UpperPaging::SystemState>);
  using SystemState = typename LowerPaging::SystemState;

  static_assert(
      ktl::is_same_v<typename LowerPaging::MapSettings, typename UpperPaging::MapSettings>);
  using MapSettings = typename LowerPaging::MapSettings;

  using MapError = arch::MapError;

  // Whether the upper and lower virtual address spaces are configured and
  // operated upon separately.
  static constexpr bool kDualSpaces = !ktl::is_same_v<LowerPaging, UpperPaging>;

  static MapSettings MmioMapSettings() {
    return {
        .access = {.readable = true, .writable = true},
        .memory = ArchMmioMemoryType(),
    };
  }

  static constexpr MapSettings NormalMapSettings(arch::AccessPermissions access) {
    return {.access = access, .memory = kArchNormalMemoryType};
  }

  // Restricts the memory out of which page tables may be allocated. A bound of
  // ktl::nullopt indicates that the corresponding default bound on the global
  // memalloc::Pool should be respected instead (i.e., the default behaviour).
  //
  // This method may be called before Init() in order to ensure that root page
  // allocation respects these bounds as well.
  //
  // In a nebulous period of early boot on x86-64, we have no guarantees on
  // what memory is mapped beyond our load image; in that case we must restrict
  // the allocation of fresh mappings out of that load image, which is where
  // this method comes in handy.
  void SetPageTableAllocationBounds(ktl::optional<uint64_t> low, ktl::optional<uint64_t> high) {
    ZX_ASSERT(!low || !high || *low <= *high);
    pt_allocation_lower_bound_ = low;
    pt_allocation_upper_bound_ = high;
  }

  // Initializes the address space, allocating the root page table(s), and
  // initializes system paging state with the arguments specified by
  // ArchCreatePagingState().
  template <typename... Args>
  void Init(Args&&... args) {
    AllocateRootPageTables();
    state_ = ArchCreatePagingState(ktl::forward<Args>(args)...);
  }

  template <bool DualSpaces = kDualSpaces, typename = ktl::enable_if_t<DualSpaces>>
  uint64_t lower_root_paddr() const {
    return lower_root_paddr_;
  }

  template <bool DualSpaces = kDualSpaces, typename = ktl::enable_if_t<DualSpaces>>
  uint64_t upper_root_paddr() const {
    return upper_root_paddr_;
  }

  template <bool DualSpaces = kDualSpaces, typename = ktl::enable_if_t<!DualSpaces>>
  uint64_t root_paddr() const {
    return lower_root_paddr_;
  }

  const SystemState& state() const { return state_; }

  // Maps the provided page-aligned physical memory region at the given virtual
  // address.
  //
  // If execute-only access is requested and the hardware does not support
  // this, the permissions will be fixed up as RX.
  //
  // If the requested virtual address range is in the upper address space, the
  // settings will also be fixed up to be global (as these ranges are intended
  // for permanent kernel mappings).
  fit::result<MapError> Map(uint64_t vaddr, uint64_t size, uint64_t paddr, MapSettings settings);

  fit::result<MapError> IdentityMap(uint64_t addr, uint64_t size, MapSettings settings) {
    return Map(addr, size, addr, settings);
  }

  // Identity maps in all RAM as RWX, as well as the global UART's registers
  // (assuming that they fit within a single page).
  void SetUpIdentityMappings() {
    IdentityMapRam();
    IdentityMapUart();
  }

  // Configures the hardware to install the address space (in an
  // architecture-specific fashion) and registers this instance as
  // gAddressSpace.
  void Install() const {
    ArchInstall();
    gAddressSpace = const_cast<AddressSpace*>(this);
  }

  // Install new lower and upper root page tables.
  template <bool DualSpaces = kDualSpaces, typename = ktl::enable_if_t<DualSpaces>>
  void InstallNewRootTables(uint64_t new_lower_root_paddr, uint64_t new_upper_root_paddr) {
    lower_root_paddr_ = new_lower_root_paddr;
    upper_root_paddr_ = new_upper_root_paddr;
    ArchInstall();
  }

  // Install a new root page table.
  template <bool DualSpaces = kDualSpaces, typename = ktl::enable_if_t<!DualSpaces>>
  void InstallNewRootTable(uint64_t new_root_paddr) {
    lower_root_paddr_ = new_root_paddr;
    ArchInstall();
  }

 private:
  static constexpr uint64_t kNumTableEntries =
      LowerPaging::kNumTableEntries<LowerPaging::kFirstLevel>;

  template <typename Paging, size_t... LevelIndex>
  static constexpr bool SameNumberOfEntries(ktl::index_sequence<LevelIndex...>) {
    return ((Paging::template kNumTableEntries<Paging::kLevels[LevelIndex]> == kNumTableEntries) &&
            ...);
  }
  // TODO(https://fxbug.dev/42083279): Uncomment.
  /*
  static_assert(
      SameNumberOfEntries<LowerPaging>(ktl::make_index_sequence<LowerPaging::kLevels.size()>()));
  static_assert(
      SameNumberOfEntries<UpperPaging>(ktl::make_index_sequence<UpperPaging::kLevels.size()>()));
  */

  using Table = hwreg::AlignedTableStorage<uint64_t, kNumTableEntries>;

  static constexpr uint64_t kLowerVirtualAddressRangeEnd =
      *LowerPaging::kLowerVirtualAddressRangeEnd;
  static constexpr uint64_t kUpperVirtualAddressRangeStart =
      *UpperPaging::kUpperVirtualAddressRangeStart;

  static_assert(LowerPaging::kExecuteOnlyAllowed == UpperPaging::kExecuteOnlyAllowed);
  static constexpr bool kExecuteOnlyAllowed = LowerPaging::kExecuteOnlyAllowed;

  void AllocateRootPageTables();
  void IdentityMapRam();
  void IdentityMapUart();

  // The architecture-specific subroutine of Install().
  //
  // Defined in //zircon/kernel/arch/$arch/phys/address-space.cc
  void ArchInstall() const;

  fit::inline_function<decltype(Table{}.direct_io())(uint64_t)> paddr_to_io_ = [](uint64_t paddr) {
    return reinterpret_cast<Table*>(paddr)->direct_io();
  };

  template <memalloc::Type AllocationType>
  ktl::optional<uint64_t> AllocatePageTable(uint64_t size, uint64_t alignment) {
    auto result = Allocation::GetPool().Allocate(
        AllocationType, size, alignment, pt_allocation_lower_bound_, pt_allocation_upper_bound_);
    if (result.is_error()) {
      return ktl::nullopt;
    }
    auto addr = static_cast<uintptr_t>(result.value());
    memset(reinterpret_cast<void*>(addr), 0, static_cast<size_t>(size));
    return addr;
  }

  // An allocator of temporary, identity-mapping page tables, used in the following cases:
  // * When kDualSpaces is true, the lower root page table.
  // * Non-root tables for pages in the lower address space.
  auto temporary_allocator() {
    return ktl::bind_front(
        &AddressSpace::AllocatePageTable<memalloc::Type::kTemporaryIdentityPageTables>, this);
  }

  // An allocator of permanent, kernel page tables, used in the following cases:
  // * When kDualSpaces is false, the root page table.
  // * Tables for pages in the upper address space.
  auto permanent_allocator() {
    return ktl::bind_front(&AddressSpace::AllocatePageTable<memalloc::Type::kKernelPageTables>,
                           this);
  }

  uint64_t lower_root_paddr_ = 0;
  uint64_t upper_root_paddr_ = 0;
  SystemState state_ = {};

  // See SetPageTableAllocationBounds() above.
  ktl::optional<uint64_t> pt_allocation_lower_bound_;
  ktl::optional<uint64_t> pt_allocation_upper_bound_;
};

#endif  // ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_ADDRESS_SPACE_H_
