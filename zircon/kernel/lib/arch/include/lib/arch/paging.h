// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_ARCH_INCLUDE_LIB_ARCH_PAGING_H_
#define ZIRCON_KERNEL_LIB_ARCH_INCLUDE_LIB_ARCH_PAGING_H_

#include <lib/fit/result.h>
#include <lib/stdcompat/algorithm.h>
#include <zircon/assert.h>

#include <array>
#include <cstddef>
#include <cstdint>
#include <iterator>
#include <limits>
#include <optional>
#include <type_traits>
#include <utility>

#include <fbl/bits.h>
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
  enum class LevelType { kExampleLevel };

  /// Captures runtime system information that feeds into translation or
  /// mapping considerations. The construction of this information is
  /// context-dependent and so is left to the user of the API.
  ///
  /// SystemState is expected to be default-constructible.
  struct SystemState {};

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
  static constexpr unsigned int kMaxPhysicalAddressSize = 64;

  /// The ordered, sequence of paging levels. This is expected to be a
  /// indexable, constexpr container of LevelType; in practice this is a
  /// std::array, or a std::span over one.
  static constexpr std::array kLevels = {LevelType::kExampleLevel};

  /// The required log2 alignment of each page table's physical address.
  static constexpr unsigned int kTableAlignmentLog2 = 0;

  /// The log2 number of page table entries for a given level.
  template <LevelType Level>
  static constexpr unsigned int kNumTableEntriesLog2 = 0;

  /// Whether access permissions may be set on non-terminal entries so as to
  /// narrow access for later levels. If false, then access permissions are
  /// only applicable to terminal entries - and those provided to
  /// `TableEntry<Level>::Set()` for non-terminal entries must be maximally
  /// permissive.
  static constexpr bool kNonTerminalAccessPermissions = false;

  /// Whether the given set of access permissions is generally valid for a
  /// page, as applied at the terminal level.
  static bool IsValidPageAccess(const SystemState& state, const AccessPermissions& access) {
    return false;
  }

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

/// A range of (zero-indexed) bits within a virtual address.
struct VirtualAddressBitRange {
  unsigned int high, low;
};

/// The page information associated with a given virtual address, returned by
/// Paging's Query API below.
struct PagingQueryResult {
  uint64_t paddr = 0;
  AccessPermissions access;
};

/// Paging provides paging-related operations for a given a set of paging
/// traits.
template <class PagingTraits>
class Paging : public PagingTraits {
 public:
  using typename PagingTraits::LevelType;
  static_assert(std::is_enum_v<LevelType> || std::is_integral_v<LevelType>);

  template <LevelType Level>
  using TableEntry = typename PagingTraits::template TableEntry<Level>;

  using PagingTraits::kLevels;
  static_assert(kLevels.size() > 0);

  using PagingTraits::kMaxPhysicalAddressSize;
  static_assert(kMaxPhysicalAddressSize > 0);
  static_assert(kMaxPhysicalAddressSize <= 64);

  /// The maximum supported physical address.
  static constexpr uint64_t kMaxPhysicalAddress = []() {
    if constexpr (kMaxPhysicalAddressSize == 64) {
      return std::numeric_limits<uint64_t>::max();
    } else {
      return (uint64_t{1u} << kMaxPhysicalAddressSize) - 1;
    }
  }();

  using PagingTraits::kTableAlignmentLog2;
  static_assert(kTableAlignmentLog2 < 64);

  template <LevelType Level>
  static constexpr unsigned int kNumTableEntriesLog2 =
      PagingTraits::template kNumTableEntriesLog2<Level>;

  static constexpr LevelType kFirstLevel = kLevels.front();

  /// The level after `Level`. Must be used in a constexpr context in
  /// which `Level` is not the last.
  template <LevelType Level>
  static constexpr LevelType kNextLevel = []() {
    static_assert(Level != kLevels.back());
    constexpr auto it = cpp20::find(kLevels.begin(), kLevels.end(), Level);
    static_assert(it != kLevels.end());
    return *std::next(it);
  }();

  /// The virtual address bit range at a given level that serves as the index
  /// into a page table at that level.
  template <LevelType Level>
  static constexpr VirtualAddressBitRange kVirtualAddressBitRange = []() {
    constexpr auto kRange = []() -> VirtualAddressBitRange {
      constexpr unsigned int kWidth = kNumTableEntriesLog2<Level>;
      if constexpr (Level == kLevels.back()) {
        return {
            .high = kTableAlignmentLog2 + kWidth - 1,
            .low = kTableAlignmentLog2,
        };
      } else {
        constexpr auto kNextRange = kVirtualAddressBitRange<kNextLevel<Level>>;
        return {
            .high = kNextRange.high + 1 + kWidth - 1,
            .low = kNextRange.high + 1,
        };
      }
    }();
    static_assert(64 > kRange.high);
    static_assert(kRange.high > kRange.low);
    static_assert(kRange.low >= kTableAlignmentLog2);
    return kRange;
  }();

  ///
  /// Main paging trait methods.
  ///
  /// I/O is abstracted via a `PaddrToTableIo` callable that maps the physical
  /// address (of a page table) to an I/O provider intended for reading from
  /// and writing to entries in that table, where offset represents the index.
  ///

  /// A diagnostic utility that returns the page information associated with a
  /// mapped virtual address `vaddr`. `fit::failed` is returned if a
  /// non-present table entry was encountered.
  template <typename PaddrToTableIo>
  static fit::result<fit::failed, PagingQueryResult> Query(uint64_t root_paddr,
                                                           PaddrToTableIo&& paddr_to_io,
                                                           uint64_t vaddr) {
    ReadonlyTerminalVisitor visitor([vaddr](const auto& terminal) {
      return PagingQueryResult{
          .paddr = TerminalAddress(terminal, vaddr),
          .access =
              AccessPermissions{
                  .readable = terminal.readable(),
                  .writable = terminal.writable(),
                  .executable = terminal.executable(),
                  .user_accessible = terminal.user_accessible(),
              },
      };
    });
    return VisitPageTables(root_paddr, std::forward<PaddrToTableIo>(paddr_to_io), visitor, vaddr);
  }

  // TODO(fxbug.dev/129344): Map().

  ///
  /// Additional primitives.
  ///
  /// The following types and methods define more abstracted paging primitives.
  /// Central to these is the notion of a "(page table) visitor". A visitor
  /// abstracts the visitation logic of page table entries encountered during
  /// a page table walk. It is represented by a type with the overloaded call
  /// signatures
  /// ```
  /// std::optional<Value>(TableIo& io,
  ///                      TableEntry<Level>& entry,
  ///                      uint64_t table_paddr);
  /// ```
  /// for some I/O provider `TableIo` and some value type `Value`, for every
  /// `Level` in `kLevels`. The I/O provider is passed to permit the visitor to
  /// make modifications when desired. When a visitor returns `std::nullopt`
  /// that is a directive to continue translation to the next entry; otherwise,
  /// the returned value signifies that the visitor is done visiting, that the
  /// walk should terminate, and that this value should be plumbed back up to
  /// whatever is orchestrating the walk. We refer to `Value` as the "value type"
  /// of a visitor.
  ///
  /// A visitor must return a non-null value before the walk terminates;
  /// otherwise, the walk panics.
  ///

  /// This type is a helper for validating that `Visitor` does indeed meet the
  /// API expectations of a page table visitor (at least when specialized with
  /// `TableIo`).
  template <typename Visitor, class TableIo>
  class VisitResult {
   private:
    template <LevelType Level>
    struct ResultAt {
      static_assert(std::is_invocable_v<Visitor, TableIo&, TableEntry<Level>&, uint64_t>);
      using type = std::invoke_result_t<Visitor, TableIo&, TableEntry<Level>&, uint64_t>;

      using value_type = typename type::value_type;
      static_assert(std::is_same_v<type, std::optional<value_type>>);
    };

   public:
    using value_type = typename ResultAt<kFirstLevel>::value_type;
    using type = std::optional<value_type>;

   private:
    template <size_t... LevelIndex>
    static constexpr bool ValueTypesCoincide(std::index_sequence<LevelIndex...>) {
      return (std::is_same_v<value_type, typename ResultAt<kLevels[LevelIndex]>::value_type> &&
              ...);
    }
    static_assert(ValueTypesCoincide(std::make_index_sequence<kLevels.size()>{}));
  };

  /// The value type of a visitor.
  template <class TableIo, typename Visitor>
  using VisitValue = typename VisitResult<TableIo, Visitor>::value_type;

  /// The type of I/O provider associated with a `PaddrToTableIo`.
  template <typename PaddrToTableIo>
  using TableIo = std::invoke_result_t<PaddrToTableIo, uint64_t>;

  /// Walk the page tables rooted at `table_paddr` for a given virtual address
  /// and visitor.
  template <typename PaddrToTableIo, typename Visitor>
  static VisitValue<Visitor, TableIo<PaddrToTableIo>> VisitPageTables(
      uint64_t table_paddr,          //
      PaddrToTableIo&& paddr_to_io,  //
      Visitor&& visitor,             //
      uint64_t vaddr) {              //
    return VisitPageTablesFrom<kFirstLevel>(table_paddr, std::forward<PaddrToTableIo>(paddr_to_io),
                                            std::forward<Visitor>(visitor), vaddr);
  }

  /// A simple read-only visitor that returns information based on the terminal
  /// entry encountered. The logic at the terminal level and visitor's return
  /// value are parameterized by `OnTerminalEntry`: called only on terminal
  /// entries this type is callable on `const TableEntry<Level>&` for each
  /// level in `kLevels` with a uniform return type. Representing its return
  /// type as `TerminalResult`, the value type of the visitor is then
  /// `fit::result<fit::failed, TerminalResult>`, where `fit::failed` is
  /// returned in the event of encountering an unexpected, non-present entry.
  template <typename OnTerminalEntry>
  class ReadonlyTerminalVisitor {
   private:
    template <LevelType Level>
    struct TerminalResultAt {
      static_assert(std::is_invocable_v<OnTerminalEntry, const TableEntry<Level>&>);
      using type = std::invoke_result_t<OnTerminalEntry, const TableEntry<Level>&>;
    };

   public:
    using TerminalResult = typename TerminalResultAt<kFirstLevel>::type;

    /// The value type of the visitor.
    using value_type = fit::result<fit::failed, TerminalResult>;

    explicit ReadonlyTerminalVisitor(OnTerminalEntry on_terminal)
        : on_terminal_(std::move(on_terminal)) {}

    template <class TableIo, LevelType Level>
    std::optional<value_type> operator()(TableIo& io, TableEntry<Level>& entry, uint64_t vaddr) {
      if (!entry.present()) {
        return fit::failed();
      }
      if (entry.terminal()) {
        return fit::ok(on_terminal_(entry));
      }
      return {};
    }

   private:
    template <size_t LevelIndex>
    static constexpr void TerminalResultsCoincide() {
      static_assert(
          std::is_same_v<TerminalResult, typename TerminalResultAt<kLevels[LevelIndex]>::type>);
    }
    template <size_t... LevelIndex>
    static constexpr bool TerminalResultsCoincide(std::index_sequence<LevelIndex...>) {
      (TerminalResultsCoincide<LevelIndex>(), ...);
      return true;
    }
    static_assert(TerminalResultsCoincide(std::make_index_sequence<kLevels.size()>{}));

    OnTerminalEntry on_terminal_;
  };

  template <typename OnTerminalEntry>
  ReadonlyTerminalVisitor(OnTerminalEntry&&)
      -> ReadonlyTerminalVisitor<std::decay_t<OnTerminalEntry>>;

 private:
  // TODO(fxbug.dev/131202): Once hwreg is constexpr-friendly, we can add a static
  // assert that zeroed entries at any level report as non-present.

  /// A helper routine for `VisitPageTables()` starting a given level, allowing
  /// for a straightforward recursive(ish) definition.
  template <LevelType Level, typename PaddrToTableIo, typename Visitor>
  static VisitValue<Visitor, TableIo<PaddrToTableIo>> VisitPageTablesFrom(
      uint64_t table_paddr,          //
      PaddrToTableIo&& paddr_to_io,  //
      Visitor&& visitor,             //
      uint64_t vaddr) {              //
    ZX_DEBUG_ASSERT(table_paddr <= kMaxPhysicalAddress);
    auto io = paddr_to_io(table_paddr);

    constexpr VirtualAddressBitRange kBitRange = kVirtualAddressBitRange<Level>;
    uint32_t entry_index = fbl::ExtractBits<kBitRange.high, kBitRange.low, uint32_t>(vaddr);

    TableEntry<Level> entry = hwreg::RegisterAddr<TableEntry<Level>>(entry_index).ReadFrom(&io);
    if (auto result = visitor(io, entry, vaddr)) {
      return *result;
    }
    if constexpr (Level == kLevels.back()) {
      ZX_PANIC("Page table visitor did not terminate the walk; there are no more levels to visit");
    } else {
      return VisitPageTablesFrom<kNextLevel<Level>>(entry.address(),
                                                    std::forward<PaddrToTableIo>(paddr_to_io),
                                                    std::forward<Visitor>(visitor), vaddr);
    }
  }

  template <LevelType Level>
  static constexpr uint64_t TerminalAddress(const TableEntry<Level>& terminal, uint64_t vaddr) {
    constexpr uint64_t kAlignmentLog2 = kVirtualAddressBitRange<Level>.low;
    return terminal.address() | fbl::ExtractBits<kAlignmentLog2 - 1, 0, uint64_t>(vaddr);
  }
};

}  // namespace arch

#endif  // ZIRCON_KERNEL_LIB_ARCH_INCLUDE_LIB_ARCH_PAGING_H_
