// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_ARCH_INCLUDE_LIB_ARCH_PAGING_H_
#define ZIRCON_KERNEL_LIB_ARCH_INCLUDE_LIB_ARCH_PAGING_H_

#include <cstdint>

#include <hwreg/bitfields.h>

namespace arch {

// Forward-declared; defined below.
struct AccessPermissions;
struct PagingSettings;

/// ExamplePagingTraits defines the "PagingTraits" API, being a coded examplar
/// intended for documentation purposes only. Parameterizing the creation of
/// virtual memory spaces, the traits are used to abstract away finicky paging
/// business logic across that differs across CPU architectures (or IOMMU
/// translation schemes). The resulting uniform API can than be used for a
/// generic approach to address translation and page mapping.
struct ExamplePagingTraits {
  /// An enum or integral type describing the levels of paging. Rather than
  /// mandate that this be an integral type, we leave room for the enum to
  /// support the use of context-specific names for each level. This is done
  /// for two reasons:
  ///
  ///   * Some architectures do not label their paging levels with numbers
  ///     (e.g., x86's "page directory pointer table"), so allowing for the use
  ///     of more recognizable names aids in readability (especially when it
  ///     comes to cross-referencing with the manual);
  ///
  ///   * Architectures that do label levels with numbers have inconsistent
  ///     conventions: for example, ARM paging levels range from -1 up to 3,
  ///     while RISC-V's range from 4 down to 0.
  ///
  enum class LevelType {};

  /// The register type representing a page table entry at a given level. It
  /// must meet the defined API below.
  template <LevelType Level>
  struct TableEntry : public hwreg::RegisterBase<TableEntry<Level>, uint64_t> {
    /// Whether the entry is present to the hardware.
    ///
    /// The reading of an entry's settings while in the 'not present' state is
    /// permitted, 'presence (to the hardware)' being regarding as orthogonal
    /// in this case. It is assumed that `ValueType{0}` is in the 'not present'
    /// state.
    constexpr bool present() const { return false; }

    /// Whether the entry is terminal and points to the ultimate, physical page
    /// (or block).
    constexpr bool terminal() const { return false; }

    /// Gives the physical address of the table at the next level or that of
    /// the ultimate, physical page (or block) if the entry is terminal.
    ///
    /// TODO(fxbug.dev/129344): document required alignment.
    constexpr uint64_t address() const { return 0; }

    /// If the entry is terminal, these accessors give the access permissions
    /// of the associated page (or block); if non-terminal, a false permission
    /// flag here means that the flag is ignored in the entries at later
    /// levels. In particular, that means that if the entry is non-terminal and
    /// `!Traits::kNonTerminalAccessPermissions` then these accessors are
    /// expected return identically true.
    ///
    /// In the case of readability, writability, and executability, these
    /// permissions constrain how the *supervisor* may access associated pages.
    /// These permissions do not a priori indicate how usermode may access them
    /// when `user_accessible()` is true; rather, the latter only indicates
    /// usermode may access them in some fashion.
    constexpr bool readable() const { return false; }
    constexpr bool writable() const { return false; }
    constexpr bool executable() const { return false; }
    constexpr bool user_accessible() const { return false; }

    /// Bulk-apply paging settings to the entry. This is done in one go as
    /// there can be interdependicies among the different aspects of entry
    /// state: these could not otherwise be captured by individual setters with
    /// an unconstrained relative call order. (For example, the setting of
    /// address could be sensitive to whether the entry is terminal.)
    ///
    /// If `settings.present` is false, all other settings should be ignored.
    ///
    /// Once a setting is applied, the corresponding getter should return
    /// reflect that identically.
    ///
    /// If `settings.terminal` is true, then it is the responsibility to have
    /// called `Traits::IsValidPageAccess()` on the provided access permissions
    /// before making this call; otherwise if
    /// `!Traits::kNonTerminalAccessPermissions` then the provided access
    /// permissions are expected to be maximally permissive.
    constexpr TableEntry& Set(const PagingSettings& settings) { return *this; }
  };

  /// The maximum number of addressable physical address bits permitted by the
  /// hardware.
  static constexpr unsigned int kMaxPhysicalAddressSize = 0;

  /// Whether access permissions may be set on non-terminal entries so as to
  /// narrow access for later levels. If false, then access permissions are
  /// only applicable to terminal entries - and those provided to
  /// `TableEntry<Level>::Set()` for non-terminal entries must be maximally
  /// permissive.
  static constexpr bool kNonTerminalAccessPermissions = false;

  /// Whether the given set of access permissions is generally valid for a
  /// page, as applied at the terminal level.
  ///
  /// TODO(fxbug.dev/129344): Should be a function of system state as well.
  static bool IsValidPageAccess(const AccessPermissions& access) { return false; }

  // TODO(fxbug.dev/129344): ...and more to support machine-independent paging.
};

/// Settings relating to the access permissions of a page or pages that map
/// through an entry.
struct AccessPermissions {
  bool readable = false;
  bool writable = false;
  bool executable = false;
  bool user_accessible = false;
};

/// As described by `ExamplePagingTraits::TableEntry<Level>::Set()`.
struct PagingSettings {
  uint64_t address = 0u;
  bool present = false;
  bool terminal = false;
  AccessPermissions access;
  // TODO(fxbug.dev/129344): global, memory type.
};

}  // namespace arch

#endif  // ZIRCON_KERNEL_LIB_ARCH_INCLUDE_LIB_ARCH_PAGING_H_
