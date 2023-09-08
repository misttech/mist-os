// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_ADDRESS_SPACE_H_
#define ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_ADDRESS_SPACE_H_

#include <zircon/assert.h>

#include <hwreg/array.h>
#include <ktl/byte.h>
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

// Perform architecture-specific address space set-up. The "Early" variant
// assumes that only the boot conditions hold and is expected to be called
// before "normal work" can proceed; otherwise, the "Late" variant assumes that
// we are in the opposite context and, in particular, that memory can be
// allocated such that it will not be clobbered before the next kernel sets up
// the address space again,
//
// In certain architectural contexts, early or late set-up will not make
// practical sense, and the associated functions may be no-ops.
void ArchSetUpAddressSpaceEarly();
void ArchSetUpAddressSpaceLate();

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
  static_assert(
      ktl::is_same_v<typename LowerPaging::SystemState, typename UpperPaging::SystemState>);
  using MemoryType = typename LowerPaging::MemoryType;
  using SystemState = typename LowerPaging::SystemState;

  using MapError = arch::MapError;
  using MapSettings = arch::MapSettings<MemoryType>;

  // An allocator of page tables, as expected by the libarch PagingTraits API.
  using PageTableAllocator =
      fit::inline_function<ktl::optional<uint64_t>(uint64_t size, uint64_t alignment)>;

  // Whether the upper and lower virtual address spaces are configured and
  // operated upon separately.
  static constexpr bool kDualSpaces = !ktl::is_same_v<LowerPaging, UpperPaging>;

  static constexpr MapSettings kMmioMapSettings = {
      .access = {.readable = true, .writable = true},
      .memory = kArchMmioMemoryType,
  };

  static constexpr MapSettings NormalMapSettings(arch::AccessPermissions access) {
    return {.access = access, .memory = kArchNormalMemoryType};
  }

  // Returns the main memalloc::Pool-backed allocator.
  static PageTableAllocator PhysPageTableAllocator(memalloc::Pool& pool = Allocation::GetPool());

  // Initializes the address space with a given page table allocator and the
  // arguments specified by ArchCreatePagingState().
  template <typename... PagingStateCreationArgs>
  void Init(PageTableAllocator allocator, PagingStateCreationArgs&&... args) {
    allocator_ = ktl::move(allocator);

    ktl::optional<uint64_t> lower_root =
        allocator_(LowerPaging::kTableSize<LowerPaging::kFirstLevel>, LowerPaging::kTableAlignment);
    ZX_ASSERT_MSG(lower_root, "failed to allocate %sroot page table", kDualSpaces ? "lower " : "");
    lower_root_paddr_ = *lower_root;

    if constexpr (kDualSpaces) {
      ktl::optional<uint64_t> upper_root = allocator_(
          UpperPaging::kTableSize<UpperPaging::kFirstLevel>, UpperPaging::kTableAlignment);
      ZX_ASSERT_MSG(upper_root, "failed to allocate upper root page table");
      upper_root_paddr_ = *upper_root;
    }

    state_ = ArchCreatePagingState(ktl::forward<PagingStateCreationArgs>(args)...);
  }

  // As above, but specifies PhysPageTableAllocator() for a page table
  // allocator.
  template <typename... Args>
  void Init(Args&&... args) {
    Init(PhysPageTableAllocator(), ktl::forward<Args>(args)...);
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
  // architecture-specific fashion).
  void ArchInstall() const;

 private:
  static constexpr uint64_t kNumTableEntries =
      LowerPaging::kNumTableEntries<LowerPaging::kFirstLevel>;

  template <typename Paging, size_t... LevelIndex>
  static constexpr bool SameNumberOfEntries(ktl::index_sequence<LevelIndex...>) {
    return ((Paging::template kNumTableEntries<Paging::kLevels[LevelIndex]> == kNumTableEntries) &&
            ...);
  }
  // TODO(fxbug.dev/133357): Uncomment.
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

  void IdentityMapRam();
  void IdentityMapUart();

  fit::inline_function<decltype(Table{}.direct_io())(uint64_t)> paddr_to_io_ = [](uint64_t paddr) {
    return reinterpret_cast<Table*>(paddr)->direct_io();
  };

  PageTableAllocator allocator_;
  uint64_t lower_root_paddr_ = 0;
  uint64_t upper_root_paddr_ = 0;
  SystemState state_ = {};
};

#endif  // ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_ADDRESS_SPACE_H_
