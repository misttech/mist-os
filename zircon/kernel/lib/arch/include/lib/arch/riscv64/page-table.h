// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_ARCH_INCLUDE_LIB_ARCH_RISCV64_PAGE_TABLE_H_
#define ZIRCON_KERNEL_LIB_ARCH_INCLUDE_LIB_ARCH_RISCV64_PAGE_TABLE_H_

#include <inttypes.h>
#include <lib/arch/paging.h>
#include <zircon/assert.h>

#include <cstdint>

#include <fbl/bits.h>
#include <hwreg/bitfields.h>

namespace arch {

enum class RiscvPagingLevel {
  k4 = 4,
  k3 = 3,
  k2 = 2,
  k1 = 1,
  k0 = 0,
};

// Captures the system state influencing RISC-V paging.
struct RiscvSystemPagingState {};

static constexpr unsigned int kRiscvMaxPhysicalAddressSize = 57;

// Whether the given access permission are valid for a RISC-V page.
static constexpr bool RiscvIsValidPageAccess(const RiscvSystemPagingState& state,
                                             const AccessPermissions& access) {
  // A page is either readable or execute-only.
  return access.readable || !access.writable;
}

template <RiscvPagingLevel Level>
class RiscvPageTableEntry : public hwreg::RegisterBase<RiscvPageTableEntry<Level>, uint64_t> {
 private:
  using SelfType = RiscvPageTableEntry;

 public:
  // When the Svpbmt extension is available, the pbmt field determines the
  // physical memory type being accessed.  Without that extension, those bits
  // must be all zero, which is also the PMA type (meaning the memory type is
  // controlled by the Physical Memory Attributes specification instead).
  enum class MemoryType : uint64_t {
    kPma = 0,  // None
    kNc = 1,   // Non-cacheable, idempotent, weakly-ordered (RVWMO), main memory
    kIo = 2,   // Non-cacheable, non-idempotent, strongly-ordered (I/O ordering), I/O
  };

  // When the Svnapot extension is available, setting the N bit means that
  // this PTE is part of a larger Naturally Aligned Power-of-2 (NAPOT) range.
  // The low bits of PPN indicate what power of 2 is the actual granularity of
  // range.  The hardware is free to assume that all the PTEs for the whole
  // range all have the N bit set and the access control bits (5..0) set the
  // same, so it can use a single TLB entry to represent the multiple PTEs in
  // the whole NAPOT range.
  DEF_BIT(63, n);  // RES0 without Svnapot extension

  // These bits are always RES0 in non-leaf PTEs.  When the Svpbmt extension
  // is available, in leaf PTEs they set the type of physical memory access.
  DEF_ENUM_FIELD(MemoryType, 62, 61, pbmt);  // RES0 without Svpbmt extension

  // Bits [60: 54] are reserved.

  // In an entry acting as the last level the physical address must have the
  // same alignment as the virtual address: 4KiB, 2MiB, 1GiB, or 512GiB.  The
  // low 12 bits that must always be zero aren't stored in the PTE.
  DEF_FIELD(53, 10, ppn);  // Physical Page Number

  // These bits are available for software use. The hardware ignores them.
  DEF_FIELD(9, 8, rsw);

  DEF_BIT(7, d);  // Dirty
  DEF_BIT(6, a);  // Accessed
  DEF_BIT(5, g);  // Global
  DEF_BIT(4, u);  // U mode access (also S mode access when sstatus.SUM clear)

  // --- (XWR all clear) means the PPN is a next-level page table.
  // -W- and XW- (R clear) are reserved for future use.
  // All other combinations have their natural meanings.
  DEF_BIT(3, x);  // eXecutable
  DEF_BIT(2, w);  // Writable
  DEF_BIT(1, r);  // Readable

  // If V is clear, then hardware ignores all other bits and they can be used
  // for software purposes.
  DEF_BIT(0, v);  // Valid

  //
  // This implements the PagingTraits::TableEntry API defined in
  // <lib/arch/paging.h>.
  //

  constexpr bool present() const { return v(); }

  constexpr uint64_t address() const { return ppn() << 12; }

  constexpr bool terminal() const { return r() || x(); }

  constexpr bool readable() const { return terminal() ? r() : true; }
  constexpr bool writable() const { return terminal() ? w() : true; }
  constexpr bool executable() const { return terminal() ? x() : true; }
  constexpr bool user_accessible() const { return terminal() ? u() : true; }

  constexpr SelfType& Set(const RiscvSystemPagingState& state, const PagingSettings& settings) {
    set_v(settings.present);
    if (!settings.present) {
      return *this;
    }

    const AccessPermissions& access = settings.access;
    if (settings.terminal) {
      ZX_DEBUG_ASSERT(RiscvIsValidPageAccess(state, access));
      set_r(access.readable)
          .set_w(access.writable)
          .set_x(access.executable)
          .set_u(access.user_accessible);
    } else {
      // Since access permissions cannot be applied to non-terminal levels to
      // constrain later ones, the provided permissions here are expected to be
      // maximally permissive.
      ZX_DEBUG_ASSERT(access.readable);
      ZX_DEBUG_ASSERT(access.writable);
      ZX_DEBUG_ASSERT(access.executable);
      ZX_DEBUG_ASSERT(access.user_accessible);

      // Intermediate levels must have the access permission bits cleared.
      set_r(false).set_w(false).set_x(false).set_u(false);
    }

    if (terminal()) {
      constexpr unsigned int kAlignmentLog2 = 12 + 9 * static_cast<unsigned int>(Level);
      ZX_DEBUG_ASSERT_MSG(
          (fbl::ExtractBits<kAlignmentLog2 - 1, 0, uint64_t>(settings.address) == 0),
          "level %u page address %#" PRIx64 " expected to have an alignment of %#" PRIx64,
          static_cast<unsigned int>(Level), settings.address, (uint64_t{1} << kAlignmentLog2));
    } else {
      ZX_DEBUG_ASSERT_MSG((fbl::ExtractBits<11, 0, uint64_t>(settings.address) == 0),
                          "page table address %#" PRIx64 "expected to be 4KiB-aligned",
                          settings.address);
    }
    ZX_DEBUG_ASSERT(
        (fbl::ExtractBits<63, kRiscvMaxPhysicalAddressSize, uint64_t>(settings.address) == 0));
    return set_ppn(settings.address >> 12);
  }
};

//
// Implementations of the PagingTraits API (defined in <lib/arch/paging.h>) for
// RISC-V.
//

struct RiscvPagingTraitsBase {
  using LevelType = RiscvPagingLevel;

  template <RiscvPagingLevel Level>
  using TableEntry = RiscvPageTableEntry<Level>;

  using SystemState = RiscvSystemPagingState;

  static constexpr unsigned int kMaxPhysicalAddressSize = kRiscvMaxPhysicalAddressSize;

  static constexpr bool kNonTerminalAccessPermissions = false;

  static constexpr bool (*IsValidPageAccess)(const RiscvSystemPagingState&,
                                             const AccessPermissions&) = RiscvIsValidPageAccess;
};

struct RiscvSv39PagingTraits : public RiscvPagingTraitsBase {};

struct RiscvSv48PagingTraits : public RiscvPagingTraitsBase {};

struct RiscvSv57PagingTraits : public RiscvPagingTraitsBase {};

}  // namespace arch

#endif  // ZIRCON_KERNEL_LIB_ARCH_INCLUDE_LIB_ARCH_RISCV64_PAGE_TABLE_H_
