// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <inttypes.h>
#include <lib/arch/arm64/page-table.h>
#include <lib/arch/paging.h>
#include <lib/arch/riscv64/page-table.h>
#include <lib/arch/x86/page-table.h>
#include <sys/types.h>

#include <array>
#include <cstddef>
#include <cstdint>
#include <cstdlib>
#include <map>
#include <memory>
#include <variant>

#include <gtest/gtest.h>
#include <hwreg/array.h>

//
// The testing strategy for the PagingTraits API implementations and its
// consumers is as follows:
//
// * First, we test the API implementations directly.
//
// * Second, we test arch::Paging against the implementations, taking the
//   latter for granted (with the confidence gained from their exercise in the
//   first round of testing).
//

namespace arch {

constexpr bool operator==(const ArmMairAttribute& l, const ArmMairAttribute& r) {
  if (l.index() != r.index()) {
    return false;
  }
  if (const auto* l_normal = std::get_if<ArmMairNormalAttribute>(&l)) {
    const auto* r_normal = std::get_if<ArmMairNormalAttribute>(&r);
    return l_normal->inner == r_normal->inner && l_normal->outer == r_normal->outer;
  }
  return std::get<ArmDeviceMemory>(l) == std::get<ArmDeviceMemory>(r);
}

}  // namespace arch

namespace {

using AccessPermissions = arch::AccessPermissions;

using ArmPagingLevel = arch::ArmAddressTranslationLevel;
using ArmPagingSettings = arch::PagingSettings<arch::ArmMairAttribute>;
using ArmSystemState = arch::ArmSystemPagingState;

using RiscvMemoryType = arch::RiscvMemoryType;
using RiscvPagingLevel = arch::RiscvPagingLevel;
using RiscvPagingSettings = arch::PagingSettings<RiscvMemoryType>;

using X86PagingSettings = arch::PagingSettings<arch::X86MemoryType>;
using X86PagingLevel = arch::X86PagingLevel;
using X86SystemState = arch::X86SystemPagingState;

template <ArmPagingLevel Level>
using ArmTableEntry =
    arch::ArmAddressTranslationDescriptor<Level, arch::ArmGranuleSize::k4KiB,
                                          arch::ArmMaximumVirtualAddressWidth::k48Bits>;

constexpr bool kBools[] = {false, true};

constexpr AccessPermissions kRWXU = {
    .readable = true,
    .writable = true,
    .executable = true,
    .user_accessible = true,
};

constexpr AccessPermissions kRX = {
    .readable = true,
    .executable = true,
};

template <ArmPagingLevel Level>
static constexpr bool kArmLevelCanBePage = Level == ArmPagingLevel::k3;

template <ArmPagingLevel Level>
static constexpr bool kArmLevelCanBeTable = Level != ArmPagingLevel::k3;

template <ArmPagingLevel Level>
static constexpr bool kArmLevelCanBeBlock =
    Level == ArmPagingLevel::k1 || Level == ArmPagingLevel::k2;

template <ArmPagingLevel Level>
static constexpr bool kArmLevelCanBeTerminal =
    kArmLevelCanBePage<Level> || kArmLevelCanBeBlock<Level>;

template <ArmPagingLevel Level>
static constexpr bool kArmLevelCanBeNonTerminal = kArmLevelCanBeTable<Level>;

template <X86PagingLevel Level>
constexpr bool kX86LevelCanBeTerminal =
    Level != X86PagingLevel::kPml5Table && Level != X86PagingLevel::kPml4Table;

template <X86PagingLevel Level>
constexpr bool kX86LevelCanBeNonTerminal = Level != X86PagingLevel::kPageTable;

//
// Tests for PagingTraits implementations.
//

template <RiscvPagingLevel Level>
void TestRiscvGetSetPresent() {
  // Presence is controlled by the V bit.
  arch::RiscvPageTableEntry<Level> entry;

  entry.set_reg_value(0).set_v(false);
  EXPECT_FALSE(entry.present());

  entry.set_reg_value(0).set_v(true);
  EXPECT_TRUE(entry.present());

  entry.set_reg_value(0).Set({}, RiscvPagingSettings{.present = false});
  EXPECT_FALSE(entry.v());
  EXPECT_FALSE(entry.present());

  entry.set_reg_value(0).Set({}, RiscvPagingSettings{.present = false});
  EXPECT_FALSE(entry.v());
  EXPECT_FALSE(entry.present());

  entry.set_reg_value(0).Set({}, RiscvPagingSettings{
                                     .present = true,
                                     .terminal = false,
                                     .access = kRWXU,
                                 });
  EXPECT_TRUE(entry.v());
  EXPECT_TRUE(entry.present());

  entry.set_reg_value(0).Set({}, RiscvPagingSettings{
                                     .present = true,
                                     .terminal = true,
                                 });
  EXPECT_TRUE(entry.v());
  EXPECT_TRUE(entry.present());
}

TEST(RiscvPagingTraitTests, GetSetPresent) {
  TestRiscvGetSetPresent<RiscvPagingLevel::k4>();
  TestRiscvGetSetPresent<RiscvPagingLevel::k3>();
  TestRiscvGetSetPresent<RiscvPagingLevel::k2>();
  TestRiscvGetSetPresent<RiscvPagingLevel::k1>();
  TestRiscvGetSetPresent<RiscvPagingLevel::k0>();
}

template <RiscvPagingLevel Level>
void TestRiscvIsValidPageAccess() {
  // Any set of page permissions must either be readable or execute-only.
  for (bool r : kBools) {
    for (bool w : kBools) {
      for (bool x : kBools) {
        for (bool u : kBools) {
          AccessPermissions access = {
              .readable = r,
              .writable = w,
              .executable = x,
              .user_accessible = u,
          };
          EXPECT_EQ(r || !w, arch::RiscvIsValidPageAccess({}, access));
        }
      }
    }
  }
}

TEST(RiscvPagingTraitTests, IsValidPageAccess) {
  TestRiscvIsValidPageAccess<RiscvPagingLevel::k4>();
  TestRiscvIsValidPageAccess<RiscvPagingLevel::k3>();
  TestRiscvIsValidPageAccess<RiscvPagingLevel::k2>();
  TestRiscvIsValidPageAccess<RiscvPagingLevel::k1>();
  TestRiscvIsValidPageAccess<RiscvPagingLevel::k0>();
}

template <RiscvPagingLevel Level>
void TestRiscvGetSetReadable() {
  //
  // Readability is controlled by the R bit.
  //

  arch::RiscvPageTableEntry<Level> entry;

  // If the entry is not terminal, then any page that maps through it can be
  // readable.
  for (bool r : kBools) {
    for (bool x : kBools) {
      entry.set_reg_value(0).set_r(r).set_x(x);
      EXPECT_EQ(entry.readable(), r || (!r && !x)) << "r=" << r << ", x=" << x;
    }
  }

  entry.set_reg_value(0).Set(
      {}, RiscvPagingSettings{
              .present = true,
              .terminal = true,
              .access =
                  AccessPermissions{
                      .readable = false,
                      .executable = true,  // Must be at least readable or executable.
                  },
          });
  EXPECT_FALSE(entry.r());
  EXPECT_FALSE(entry.readable());

  entry.set_reg_value(0).Set({}, RiscvPagingSettings{
                                     .present = true,
                                     .terminal = true,
                                     .access = AccessPermissions{.readable = true},
                                 });
  EXPECT_TRUE(entry.r());
  EXPECT_TRUE(entry.readable());

  entry.set_reg_value(0).Set({}, RiscvPagingSettings{
                                     .present = true,
                                     .terminal = false,
                                     .access = kRWXU,
                                 });
  EXPECT_FALSE(entry.r());
  EXPECT_TRUE(entry.readable());
}

TEST(RiscvPagingTraitTests, GetSetReadable) {
  TestRiscvGetSetReadable<RiscvPagingLevel::k4>();
  TestRiscvGetSetReadable<RiscvPagingLevel::k3>();
  TestRiscvGetSetReadable<RiscvPagingLevel::k2>();
  TestRiscvGetSetReadable<RiscvPagingLevel::k1>();
  TestRiscvGetSetReadable<RiscvPagingLevel::k0>();
}

template <RiscvPagingLevel Level>
void TestRiscvGetSetWritable() {
  //
  // Writability is controlled by the W bit.
  //
  arch::RiscvPageTableEntry<Level> entry;

  // If the entry is not terminal, then any page that maps through it can be
  // writable.
  for (bool r : kBools) {
    for (bool w : kBools) {
      for (bool x : kBools) {
        entry.set_reg_value(0).set_r(r).set_w(w).set_x(x);
        EXPECT_EQ(entry.writable(), w || (!r && !x)) << "r=" << r << ", w=" << w << ", x=" << x;
      }
    }
  }

  entry.set_reg_value(0).Set(
      {}, RiscvPagingSettings{
              .present = true,
              .terminal = true,
              .access =
                  AccessPermissions{
                      .readable = true,  // Must be at least readable or executable.
                      .writable = false,
                  },
          });
  EXPECT_FALSE(entry.w());
  EXPECT_FALSE(entry.writable());

  entry.set_reg_value(0).Set(
      {}, RiscvPagingSettings{
              .present = true,
              .terminal = true,
              .access =
                  AccessPermissions{
                      .readable = true,  // Must be at least readable or executable.
                      .writable = true,
                  },
          });
  EXPECT_TRUE(entry.w());
  EXPECT_TRUE(entry.writable());

  entry.set_reg_value(0).Set({}, RiscvPagingSettings{
                                     .present = true,
                                     .terminal = false,
                                     .access = kRWXU,
                                 });
  EXPECT_FALSE(entry.w());
  EXPECT_TRUE(entry.writable());
}

TEST(RiscvPagingTraitTests, GetSetWritable) {
  TestRiscvGetSetWritable<RiscvPagingLevel::k4>();
  TestRiscvGetSetWritable<RiscvPagingLevel::k3>();
  TestRiscvGetSetWritable<RiscvPagingLevel::k2>();
  TestRiscvGetSetWritable<RiscvPagingLevel::k1>();
  TestRiscvGetSetWritable<RiscvPagingLevel::k0>();
}

template <RiscvPagingLevel Level>
void TestRiscvGetSetExecutable() {
  //
  // Executability is controlled by the X bit.
  //
  arch::RiscvPageTableEntry<Level> entry;

  // If the entry is not terminal, then any page that maps through it can be
  // executable.
  for (bool r : kBools) {
    for (bool x : kBools) {
      entry.set_reg_value(0).set_r(r).set_x(x);
      EXPECT_EQ(entry.executable(), x || (!r && !x)) << "r=" << r << ", x=" << x;
    }
  }

  entry.set_reg_value(0).Set(
      {}, RiscvPagingSettings{
              .present = true,
              .terminal = true,
              .access =
                  AccessPermissions{
                      .readable = true,  // Must be at least readable or executable.
                      .executable = false,
                  },
          });
  EXPECT_FALSE(entry.x());
  EXPECT_FALSE(entry.executable());

  entry.set_reg_value(0).Set({}, RiscvPagingSettings{
                                     .present = true,
                                     .terminal = true,
                                     .access = AccessPermissions{.executable = true},
                                 });
  EXPECT_TRUE(entry.x());
  EXPECT_TRUE(entry.executable());

  entry.set_reg_value(0).Set({}, RiscvPagingSettings{
                                     .present = true,
                                     .terminal = false,
                                     .access = kRWXU,
                                 });
  EXPECT_FALSE(entry.x());
  EXPECT_TRUE(entry.executable());
}

TEST(RiscvPagingTraitTests, GetSetExecutable) {
  TestRiscvGetSetExecutable<RiscvPagingLevel::k4>();
  TestRiscvGetSetExecutable<RiscvPagingLevel::k3>();
  TestRiscvGetSetExecutable<RiscvPagingLevel::k2>();
  TestRiscvGetSetExecutable<RiscvPagingLevel::k1>();
  TestRiscvGetSetExecutable<RiscvPagingLevel::k0>();
}

template <RiscvPagingLevel Level>
void TestRiscvGetSetUserAccesible() {
  //
  // Usermode accessibility is controlled by the U bit.
  //
  arch::RiscvPageTableEntry<Level> entry;

  // If the entry is not terminal, then any page that maps through it can be
  // user-accessible.
  for (bool r : kBools) {
    for (bool x : kBools) {
      for (bool u : kBools) {
        entry.set_reg_value(0).set_r(r).set_x(x).set_u(u);
        EXPECT_EQ(entry.user_accessible(), u || (!r && !x))
            << "r=" << r << ", x=" << x << ", u=" << u;
      }
    }
  }

  entry.set_reg_value(0).Set({}, RiscvPagingSettings{
                                     .present = true,
                                     .terminal = true,
                                     .access =
                                         AccessPermissions{
                                             .readable = true,
                                             .user_accessible = false,
                                         },
                                 });
  EXPECT_FALSE(entry.u());
  EXPECT_FALSE(entry.user_accessible());

  entry.set_reg_value(0).Set({}, RiscvPagingSettings{
                                     .present = true,
                                     .terminal = true,
                                     .access =
                                         AccessPermissions{
                                             .readable = true,
                                             .user_accessible = true,
                                         },
                                 });
  EXPECT_TRUE(entry.u());
  EXPECT_TRUE(entry.user_accessible());

  //
  // There are no intermediate knobs to disable permissions.
  //

  entry.set_reg_value(0).Set({}, RiscvPagingSettings{
                                     .present = true,
                                     .terminal = false,
                                     .access = kRWXU,
                                 });
  EXPECT_FALSE(entry.u());
  EXPECT_TRUE(entry.user_accessible());
}

TEST(RiscvPagingTraitTests, GetSetUserAccesible) {
  TestRiscvGetSetUserAccesible<RiscvPagingLevel::k4>();
  TestRiscvGetSetUserAccesible<RiscvPagingLevel::k3>();
  TestRiscvGetSetUserAccesible<RiscvPagingLevel::k2>();
  TestRiscvGetSetUserAccesible<RiscvPagingLevel::k1>();
  TestRiscvGetSetUserAccesible<RiscvPagingLevel::k0>();
}

template <RiscvPagingLevel Level>
void TestRiscvGetSetTerminal() {
  // Terminality is indicated by the R or X bit.
  arch::RiscvPageTableEntry<Level> entry;

  for (bool r : kBools) {
    for (bool x : kBools) {
      entry.set_reg_value(0).set_r(r).set_x(x);
      EXPECT_EQ(entry.terminal(), r || x) << "r=" << r << ", x=" << x;
    }
  }

  entry.set_reg_value(0).set_r(false).set_x(false);
  EXPECT_FALSE(entry.terminal());

  entry.set_reg_value(0).set_r(true).set_x(false);
  EXPECT_TRUE(entry.terminal());

  entry.set_reg_value(0).set_r(false).set_x(true);
  EXPECT_TRUE(entry.terminal());

  entry.set_reg_value(0).set_r(true).set_x(true);
  EXPECT_TRUE(entry.terminal());

  //
  // Setting as non-terminal should always result in the R, W, and X bits being
  // unset.
  //

  for (unsigned int r = 0; r <= 1; ++r) {
    for (unsigned int w = 0; w <= 1; ++w) {
      for (unsigned int x = 0; x <= 1; ++x) {
        entry.set_reg_value(0).set_r(r).set_w(w).set_x(x).Set({}, RiscvPagingSettings{
                                                                      .present = true,
                                                                      .terminal = false,
                                                                      .access = kRWXU,
                                                                  });
        EXPECT_FALSE(entry.terminal());
        EXPECT_FALSE(entry.r());
        EXPECT_FALSE(entry.w());
        EXPECT_FALSE(entry.x());
      }
    }
  }

  //
  // Setting as terminal requires readability or executability.
  //

  entry.set_reg_value(0).Set({}, RiscvPagingSettings{
                                     .present = true,
                                     .terminal = true,
                                     .access =
                                         AccessPermissions{
                                             .readable = true,
                                             .executable = false,
                                         },
                                 });
  EXPECT_TRUE(entry.terminal());

  entry.set_reg_value(0).Set({}, RiscvPagingSettings{
                                     .present = true,
                                     .terminal = true,
                                     .access =
                                         AccessPermissions{
                                             .readable = false,
                                             .executable = true,
                                         },
                                 });
  EXPECT_TRUE(entry.terminal());

  entry.set_reg_value(0).Set({}, RiscvPagingSettings{
                                     .present = true,
                                     .terminal = true,
                                     .access =
                                         AccessPermissions{
                                             .readable = true,
                                             .executable = true,
                                         },
                                 });
  EXPECT_TRUE(entry.terminal());
}

TEST(RiscvPagingTraitTests, GetSetTerminal) {
  TestRiscvGetSetTerminal<RiscvPagingLevel::k4>();
  TestRiscvGetSetTerminal<RiscvPagingLevel::k3>();
  TestRiscvGetSetTerminal<RiscvPagingLevel::k2>();
  TestRiscvGetSetTerminal<RiscvPagingLevel::k1>();
  TestRiscvGetSetTerminal<RiscvPagingLevel::k0>();
}

template <RiscvPagingLevel Level>
void TestRiscvGetSetAddress() {
  // The next table or page address is given via the PPN field.
  arch::RiscvPageTableEntry<Level> entry;

  entry.set_reg_value(0).set_ppn(0xaaaa'aaaa);
  EXPECT_EQ(0x0aaa'aaaa'a000u, entry.address());

  constexpr std::array<uint64_t, 5> kAddrs = {
      0x0000'0000'0000'1000u,  // 4KiB-aligned
      0x0000'0000'0020'0000u,  // 2MiB-aligned
      0x0000'0000'4000'0000u,  // 1GiB-aligned
      0x0000'0080'0000'0000u,  // 512GiB-aligned
      0x0001'0000'0000'0000u,  // 256TiB-aligned
  };

  // Each address in kAddrs should be a valid table address.
  for (uint64_t addr : kAddrs) {
    entry.set_reg_value(0).Set({}, RiscvPagingSettings{
                                       .address = addr,
                                       .present = true,
                                       .terminal = false,
                                       .access = kRWXU,
                                   });
    EXPECT_EQ(addr >> 12, entry.ppn());
    EXPECT_EQ(addr, entry.address());
  }

  // The index for the first valid page address (per alignment constraints).
  constexpr size_t kFirstSupported = static_cast<size_t>(Level);

  for (size_t i = kFirstSupported; i < kAddrs.size(); ++i) {
    uint64_t addr = kAddrs[i];
    entry.set_reg_value(0).Set({}, RiscvPagingSettings{
                                       .address = addr,
                                       .present = true,
                                       .terminal = true,
                                       .access = AccessPermissions{.readable = true},
                                   });
    EXPECT_EQ(addr >> 12, entry.ppn());
    EXPECT_EQ(addr, entry.address());
  }
}

TEST(RiscvPagingTraitTests, GetSetAddress) {
  TestRiscvGetSetAddress<RiscvPagingLevel::k4>();
  TestRiscvGetSetAddress<RiscvPagingLevel::k3>();
  TestRiscvGetSetAddress<RiscvPagingLevel::k2>();
  TestRiscvGetSetAddress<RiscvPagingLevel::k1>();
  TestRiscvGetSetAddress<RiscvPagingLevel::k0>();
}

template <RiscvPagingLevel Level>
void TestRiscvGetSetMemory() {
  //
  // The memory type is given via the PBMT field.
  //

  arch::RiscvPageTableEntry<Level> entry;

  // Memory() can only be called on a terminal entry.
  entry = entry.set_reg_value(0).Set({}, RiscvPagingSettings{
                                             .present = true,
                                             .terminal = true,
                                             .access = {.readable = true},
                                         });
  entry.set_pbmt(RiscvMemoryType::kPma);
  EXPECT_EQ(RiscvMemoryType::kPma, entry.Memory({}));
  entry.set_pbmt(RiscvMemoryType::kNc);
  EXPECT_EQ(RiscvMemoryType::kNc, entry.Memory({}));
  entry.set_pbmt(RiscvMemoryType::kIo);
  EXPECT_EQ(RiscvMemoryType::kIo, entry.Memory({}));

  entry.set_reg_value(0).Set({}, RiscvPagingSettings{
                                     .present = true,
                                     .terminal = true,
                                     .access = {.readable = true},
                                     .memory = RiscvMemoryType::kPma,
                                 });
  EXPECT_EQ(RiscvMemoryType::kPma, entry.pbmt());

  entry.set_reg_value(0).Set({}, RiscvPagingSettings{
                                     .present = true,
                                     .terminal = true,
                                     .access = {.readable = true},
                                     .memory = RiscvMemoryType::kNc,
                                 });
  EXPECT_EQ(RiscvMemoryType::kNc, entry.pbmt());

  entry.set_reg_value(0).Set({}, RiscvPagingSettings{
                                     .present = true,
                                     .terminal = true,
                                     .access = {.readable = true},
                                     .memory = RiscvMemoryType::kIo,
                                 });
  EXPECT_EQ(RiscvMemoryType::kIo, entry.pbmt());

  //
  // If the entry is non-terminal, the setting of memory should have no effect.
  //

  entry.set_reg_value(0).Set({}, RiscvPagingSettings{
                                     .present = true,
                                     .terminal = false,
                                     .memory = RiscvMemoryType::kNc,
                                 });
  EXPECT_EQ(RiscvMemoryType::kPma, entry.pbmt());

  entry.set_reg_value(0).Set({}, RiscvPagingSettings{
                                     .present = true,
                                     .terminal = false,
                                     .memory = RiscvMemoryType::kIo,
                                 });
  EXPECT_EQ(RiscvMemoryType::kPma, entry.pbmt());
}

TEST(RiscvPagingTraitTests, GetSetMemory) {
  TestRiscvGetSetMemory<RiscvPagingLevel::k4>();
  TestRiscvGetSetMemory<RiscvPagingLevel::k3>();
  TestRiscvGetSetMemory<RiscvPagingLevel::k2>();
  TestRiscvGetSetMemory<RiscvPagingLevel::k1>();
  TestRiscvGetSetMemory<RiscvPagingLevel::k0>();
}

template <RiscvPagingLevel Level>
void TestRiscvGetSetAccessed() {
  //
  // Whether the entry was 'accessed' is given via the A field.
  //

  arch::RiscvPageTableEntry<Level> entry;

  for (bool t : kBools) {
    for (bool a : kBools) {
      entry.set_reg_value(0).set_r(t).set_a(a);
      EXPECT_EQ(t && a, entry.accessed());

      entry.set_reg_value(0).Set({}, RiscvPagingSettings{
                                         .present = true,
                                         .terminal = t,
                                         .access = {.readable = true},
                                         .accessed = a,
                                     });
      EXPECT_EQ(t && a, entry.accessed()) << "t = " << t << "; a = " << a;
    }
  }
}

TEST(RiscvPagingTraitTests, GetSetAccessed) {
  TestRiscvGetSetAccessed<RiscvPagingLevel::k4>();
  TestRiscvGetSetAccessed<RiscvPagingLevel::k3>();
  TestRiscvGetSetAccessed<RiscvPagingLevel::k2>();
  TestRiscvGetSetAccessed<RiscvPagingLevel::k1>();
  TestRiscvGetSetAccessed<RiscvPagingLevel::k0>();
}

template <X86PagingLevel Level>
void TestX86GetSetPresent() {
  // Presence is controlled by the P bit.
  arch::X86PagingStructure<Level> entry;

  entry.set_reg_value(0).set_p(false);
  EXPECT_FALSE(entry.set_reg_value(0).present());

  entry.set_reg_value(0).set_p(true);
  EXPECT_TRUE(entry.present());

  if constexpr (kX86LevelCanBeNonTerminal<Level>) {
    entry.set_reg_value(0).Set(
        {}, X86PagingSettings{.present = false, .terminal = false, .access = kRX});
    EXPECT_FALSE(entry.p());
    EXPECT_FALSE(entry.present());

    entry.set_reg_value(0).Set(
        {}, X86PagingSettings{.present = true, .terminal = false, .access = kRX});
    EXPECT_TRUE(entry.p());
    EXPECT_TRUE(entry.present());
  }

  if constexpr (kX86LevelCanBeTerminal<Level>) {
    entry.set_reg_value(0).Set(
        {}, X86PagingSettings{.present = false, .terminal = true, .access = kRX});
    EXPECT_FALSE(entry.p());
    EXPECT_FALSE(entry.present());

    entry.set_reg_value(0).Set({},
                               X86PagingSettings{.present = true, .terminal = true, .access = kRX});
    EXPECT_TRUE(entry.p());
    EXPECT_TRUE(entry.present());
  }
}

TEST(X86PagingTraitTests, GetSetPresent) {
  TestX86GetSetPresent<X86PagingLevel::kPml5Table>();
  TestX86GetSetPresent<X86PagingLevel::kPml4Table>();
  TestX86GetSetPresent<X86PagingLevel::kPageDirectoryPointerTable>();
  TestX86GetSetPresent<X86PagingLevel::kPageDirectory>();
  TestX86GetSetPresent<X86PagingLevel::kPageTable>();
}

template <X86PagingLevel Level>
void TestX86CheckAccessPermissions() {
  for (bool r : kBools) {
    for (bool w : kBools) {
      for (bool x : kBools) {
        for (bool u : kBools) {
          AccessPermissions access = {
              .readable = r,
              .writable = w,
              .executable = x,
              .user_accessible = u,
          };
          EXPECT_EQ(r, arch::X86IsValidPageAccess({}, access));
        }
      }
    }
  }
}

TEST(X86PagingTraitTests, CheckAccessPermissions) {
  TestX86CheckAccessPermissions<X86PagingLevel::kPml5Table>();
  TestX86CheckAccessPermissions<X86PagingLevel::kPml4Table>();
  TestX86CheckAccessPermissions<X86PagingLevel::kPageDirectoryPointerTable>();
  TestX86CheckAccessPermissions<X86PagingLevel::kPageDirectory>();
  TestX86CheckAccessPermissions<X86PagingLevel::kPageTable>();
}

template <X86PagingLevel Level>
void TestX86GetSetReadable() {
  //
  // Readability is a default.
  //
  arch::X86PagingStructure<Level> entry;

  entry.set_reg_value(0);
  EXPECT_TRUE(entry.readable());

  if constexpr (kX86LevelCanBeTerminal<Level>) {
    entry.set_reg_value(0).Set({}, X86PagingSettings{
                                       .present = true,
                                       .terminal = true,
                                       .access = kRX,
                                   });
    EXPECT_TRUE(entry.readable());
  }
}

TEST(X86PagingTraitTests, GetSetReadable) {
  TestX86GetSetReadable<X86PagingLevel::kPml5Table>();
  TestX86GetSetReadable<X86PagingLevel::kPml4Table>();
  TestX86GetSetReadable<X86PagingLevel::kPageDirectoryPointerTable>();
  TestX86GetSetReadable<X86PagingLevel::kPageDirectory>();
  TestX86GetSetReadable<X86PagingLevel::kPageTable>();
}

template <X86PagingLevel Level>
void TestX86GetSetWritable() {
  //
  // Writability is controlled by the R/W bit.
  //

  arch::X86PagingStructure<Level> entry;

  entry.set_reg_value(0).set_r_w(false);
  EXPECT_FALSE(entry.writable());

  entry.set_reg_value(0).set_r_w(true);
  EXPECT_TRUE(entry.writable());

  if constexpr (kX86LevelCanBeNonTerminal<Level>) {
    entry.set_reg_value(0).set_r_w(true).Set(
        {},
        X86PagingSettings{
            .present = true,
            .terminal = false,
            .access = AccessPermissions{.readable = true, .writable = false, .executable = true},
        });
    EXPECT_FALSE(entry.r_w());
    EXPECT_FALSE(entry.writable());

    entry.set_reg_value(0).Set(
        {}, X86PagingSettings{
                .present = true,
                .terminal = false,
                .access = AccessPermissions{.readable = true, .writable = true, .executable = true},
            });
    EXPECT_TRUE(entry.r_w());
    EXPECT_TRUE(entry.writable());
  }

  if constexpr (kX86LevelCanBeTerminal<Level>) {
    entry.set_reg_value(0).set_r_w(true).Set(
        {},
        X86PagingSettings{
            .present = true,
            .terminal = true,
            .access = AccessPermissions{.readable = true, .writable = false, .executable = true},
        });
    EXPECT_FALSE(entry.r_w());
    EXPECT_FALSE(entry.writable());

    entry.set_reg_value(0).Set(
        {}, X86PagingSettings{
                .present = true,
                .terminal = true,
                .access = AccessPermissions{.readable = true, .writable = true, .executable = true},
            });
    EXPECT_TRUE(entry.r_w());
    EXPECT_TRUE(entry.writable());
  }
}

TEST(X86PagingTraitTests, GetSetWritable) {
  TestX86GetSetWritable<X86PagingLevel::kPml5Table>();
  TestX86GetSetWritable<X86PagingLevel::kPml4Table>();
  TestX86GetSetWritable<X86PagingLevel::kPageDirectoryPointerTable>();
  TestX86GetSetWritable<X86PagingLevel::kPageDirectory>();
  TestX86GetSetWritable<X86PagingLevel::kPageTable>();
}

template <X86PagingLevel Level>
void TestX86GetSetExecutable() {
  //
  // Executability is controlled by the XD bit.
  //

  arch::X86PagingStructure<Level> entry;

  entry.set_reg_value(0).set_xd(true);
  EXPECT_FALSE(entry.executable());

  entry.set_reg_value(0).set_xd(false);
  EXPECT_TRUE(entry.executable());

  if constexpr (kX86LevelCanBeNonTerminal<Level>) {
    entry.set_reg_value(0).set_xd(false).Set(
        {}, X86PagingSettings{
                .present = true,
                .terminal = false,
                .access = AccessPermissions{.readable = true, .executable = false},
            });
    EXPECT_TRUE(entry.xd());
    EXPECT_FALSE(entry.executable());

    entry.set_reg_value(0).set_xd(false).Set({}, X86PagingSettings{
                                                     .present = true,
                                                     .terminal = false,
                                                     .access = kRX,
                                                 });
    EXPECT_FALSE(entry.xd());
  }

  if constexpr (kX86LevelCanBeTerminal<Level>) {
    entry.set_reg_value(0).set_xd(false).Set(
        {}, X86PagingSettings{
                .present = true,
                .terminal = true,
                .access = AccessPermissions{.readable = true, .executable = false},
            });
    EXPECT_TRUE(entry.xd());
    EXPECT_FALSE(entry.executable());

    entry.set_reg_value(0).set_xd(false).Set({}, X86PagingSettings{
                                                     .present = true,
                                                     .terminal = true,
                                                     .access = kRX,
                                                 });
    EXPECT_FALSE(entry.xd());
    EXPECT_TRUE(entry.executable());
  }
}

TEST(X86PagingTraitTests, GetSetExecutable) {
  TestX86GetSetExecutable<X86PagingLevel::kPml5Table>();
  TestX86GetSetExecutable<X86PagingLevel::kPml4Table>();
  TestX86GetSetExecutable<X86PagingLevel::kPageDirectoryPointerTable>();
  TestX86GetSetExecutable<X86PagingLevel::kPageDirectory>();
  TestX86GetSetExecutable<X86PagingLevel::kPageTable>();
}

template <X86PagingLevel Level>
void TestX86GetSetUserAccessible() {
  //
  // Usermode accessibility is controlled by the U/S bit.
  //

  arch::X86PagingStructure<Level> entry;

  entry.set_reg_value(0).set_u_s(false);
  EXPECT_FALSE(entry.user_accessible());

  entry.set_reg_value(0).set_u_s(true);
  EXPECT_TRUE(entry.user_accessible());

  if constexpr (kX86LevelCanBeNonTerminal<Level>) {
    entry.set_reg_value(0).set_u_s(true).Set(
        {},
        X86PagingSettings{
            .present = true,
            .terminal = false,
            .access =
                AccessPermissions{.readable = true, .executable = true, .user_accessible = false},
        });
    EXPECT_FALSE(entry.u_s());
    EXPECT_FALSE(entry.user_accessible());

    entry.set_reg_value(0).Set({}, X86PagingSettings{
                                       .present = true,
                                       .terminal = false,
                                       .access = AccessPermissions{.readable = true,
                                                                   .executable = true,
                                                                   .user_accessible = true},
                                   });
    EXPECT_TRUE(entry.u_s());
    EXPECT_TRUE(entry.user_accessible());
  }

  if constexpr (kX86LevelCanBeTerminal<Level>) {
    entry.set_reg_value(0).set_u_s(true).Set(
        {},
        X86PagingSettings{
            .present = true,
            .terminal = true,
            .access =
                AccessPermissions{.readable = true, .executable = true, .user_accessible = false},
        });
    EXPECT_FALSE(entry.u_s());
    EXPECT_FALSE(entry.user_accessible());

    entry.set_reg_value(0).Set({}, X86PagingSettings{
                                       .present = true,
                                       .terminal = true,
                                       .access =
                                           AccessPermissions{
                                               .readable = true,
                                               .executable = true,
                                               .user_accessible = true,
                                           },
                                   });
    EXPECT_TRUE(entry.u_s());
    EXPECT_TRUE(entry.user_accessible());
  }
}

TEST(X86PagingTraitTests, GetSetUserAccessible) {
  TestX86GetSetUserAccessible<X86PagingLevel::kPml5Table>();
  TestX86GetSetUserAccessible<X86PagingLevel::kPml4Table>();
  TestX86GetSetUserAccessible<X86PagingLevel::kPageDirectoryPointerTable>();
  TestX86GetSetUserAccessible<X86PagingLevel::kPageDirectory>();
  TestX86GetSetUserAccessible<X86PagingLevel::kPageTable>();
}

template <X86PagingLevel Level>
void TestX86GetSetTerminal() {
  // The PS bit controls terminality for PDPT or PD entries.
  constexpr bool kPdptOrkPd = Level == X86PagingLevel::kPageDirectoryPointerTable ||
                              Level == X86PagingLevel::kPageDirectory;

  arch::X86PagingStructure<Level> entry;

  // Page table entries are terminal by default.
  if constexpr (Level == X86PagingLevel::kPageTable) {
    entry.set_reg_value(0);
    EXPECT_TRUE(entry.terminal());
  }

  if constexpr (kX86LevelCanBeNonTerminal<Level>) {
    entry.set_reg_value(0).Set(
        {}, X86PagingSettings{.present = true, .terminal = false, .access = kRX});
    EXPECT_FALSE(entry.terminal());
    if constexpr (kPdptOrkPd) {
      EXPECT_FALSE(*entry.ps());
    } else {
      EXPECT_EQ(std::nullopt, entry.ps());
    }
  }

  if constexpr (kX86LevelCanBeTerminal<Level>) {
    entry.set_reg_value(0).Set({},
                               X86PagingSettings{.present = true, .terminal = true, .access = kRX});
    EXPECT_TRUE(entry.terminal());
    if constexpr (kPdptOrkPd) {
      EXPECT_TRUE(*entry.ps());
    } else {
      EXPECT_EQ(std::nullopt, entry.ps());
    }
  }
}

TEST(X86PagingTraitTests, GetSetTerminal) {
  TestX86GetSetTerminal<X86PagingLevel::kPml5Table>();
  TestX86GetSetTerminal<X86PagingLevel::kPml4Table>();
  TestX86GetSetTerminal<X86PagingLevel::kPageDirectoryPointerTable>();
  TestX86GetSetTerminal<X86PagingLevel::kPageDirectory>();
  TestX86GetSetTerminal<X86PagingLevel::kPageTable>();
}

template <X86PagingLevel Level>
void TestX86GetSetAddress() {
  arch::X86PagingStructure<Level> entry;

  constexpr std::array<uint64_t, 5> kAddrs = {
      0x0000'0000'0000'1000u,  // 4KiB-aligned
      0x0000'0000'0020'0000u,  // 2MiB-aligned
      0x0000'0000'4000'0000u,  // 1GiB-aligned
      0x0000'0080'0000'0000u,  // 512GiB-aligned
      0x0001'0000'0000'0000u,  // 256TiB-aligned
  };

  if constexpr (kX86LevelCanBeNonTerminal<Level>) {
    // Each address in kAddrs should be a valid table address.
    for (size_t i = 0; i < kAddrs.size(); ++i) {
      uint64_t addr = kAddrs[i];
      entry.set_reg_value(0).Set({}, X86PagingSettings{
                                         .address = addr,
                                         .present = true,
                                         .terminal = false,
                                         .access = kRX,
                                     });
      EXPECT_EQ(addr, entry.address());
    }
  }

  if constexpr (kX86LevelCanBeTerminal<Level>) {
    // The index for the first valid page address (per alignment constraints).
    constexpr size_t kFirstSupported = Level == X86PagingLevel::kPageTable       ? 0
                                       : Level == X86PagingLevel::kPageDirectory ? 1
                                                                                 : 2;
    for (size_t i = kFirstSupported; i < kAddrs.size(); ++i) {
      uint64_t addr = kAddrs[i];
      entry.set_reg_value(0).Set({}, X86PagingSettings{
                                         .address = addr,
                                         .present = true,
                                         .terminal = true,
                                         .access = kRX,
                                     });
      EXPECT_EQ(addr, entry.address());
    }
  }
}

TEST(X86PagingTraitTests, GetSetAddress) {
  TestX86GetSetAddress<X86PagingLevel::kPml5Table>();
  TestX86GetSetAddress<X86PagingLevel::kPml4Table>();
  TestX86GetSetAddress<X86PagingLevel::kPageDirectoryPointerTable>();
  TestX86GetSetAddress<X86PagingLevel::kPageDirectory>();
  TestX86GetSetAddress<X86PagingLevel::kPageTable>();
}

template <X86PagingLevel Level>
void TestX86GetSetAccessed() {
  arch::X86PagingStructure<Level> entry;

  entry.set_reg_value(0).set_a(false);
  EXPECT_FALSE(entry.accessed());

  entry.set_reg_value(0).set_a(true);
  EXPECT_TRUE(entry.accessed());

  if constexpr (kX86LevelCanBeNonTerminal<Level>) {
    entry.set_reg_value(0).Set({}, X86PagingSettings{
                                       .present = true,
                                       .terminal = false,
                                       .accessed = false,
                                   });
    EXPECT_FALSE(entry.accessed());

    entry.set_reg_value(0).Set({}, X86PagingSettings{
                                       .present = true,
                                       .terminal = false,
                                       .accessed = true,
                                   });
    EXPECT_TRUE(entry.accessed());
  }

  if constexpr (kX86LevelCanBeTerminal<Level>) {
    entry.set_reg_value(0).Set({}, X86PagingSettings{
                                       .present = true,
                                       .terminal = true,
                                       .accessed = false,
                                   });
    EXPECT_FALSE(entry.accessed());

    entry.set_reg_value(0).Set({}, X86PagingSettings{
                                       .present = true,
                                       .terminal = true,
                                       .accessed = true,
                                   });
    EXPECT_TRUE(entry.accessed());
  }
}

TEST(X86PagingTraitTests, GetSetAccessed) {
  TestX86GetSetAccessed<X86PagingLevel::kPml5Table>();
  TestX86GetSetAccessed<X86PagingLevel::kPml4Table>();
  TestX86GetSetAccessed<X86PagingLevel::kPageDirectoryPointerTable>();
  TestX86GetSetAccessed<X86PagingLevel::kPageDirectory>();
  TestX86GetSetAccessed<X86PagingLevel::kPageTable>();
}

template <ArmPagingLevel Level>
void TestArmGetSetPresent() {
  ArmTableEntry<Level> entry;

  entry.set_reg_value(0).set_valid(false);
  EXPECT_FALSE(entry.present());

  entry.set_reg_value(0).set_valid(true);
  EXPECT_TRUE(entry.present());

  entry.set_reg_value(0).set_valid(true).Set({}, ArmPagingSettings{.present = false});
  EXPECT_FALSE(entry.valid());
  EXPECT_FALSE(entry.present());

  if constexpr (kArmLevelCanBeNonTerminal<Level>) {
    entry.set_reg_value(0).set_valid(false).Set({}, ArmPagingSettings{
                                                        .present = true,
                                                        .terminal = false,
                                                    });
    EXPECT_TRUE(entry.valid());
    EXPECT_TRUE(entry.present());
  }

  if constexpr (kArmLevelCanBeTerminal<Level>) {
    entry.set_reg_value(0).set_valid(false).Set({}, ArmPagingSettings{
                                                        .present = true,
                                                        .terminal = true,
                                                    });
    EXPECT_TRUE(entry.valid());
    EXPECT_TRUE(entry.present());
  }
}

TEST(ArmPagingTraitTests, GetSetPresent) {
  TestArmGetSetPresent<ArmPagingLevel::k0>();
  TestArmGetSetPresent<ArmPagingLevel::k1>();
  TestArmGetSetPresent<ArmPagingLevel::k2>();
  TestArmGetSetPresent<ArmPagingLevel::k3>();
}

template <ArmPagingLevel Level>
void TestArmDescriptorTypes() {
  using Format = arch::ArmAddressTranslationDescriptorFormat;

  ArmTableEntry<Level> entry;

  // The table-or-page format represents a page at the last level and a table
  // at all others.
  entry.set_reg_value(0).set_format(Format::kTableOrPage);
  if constexpr (Level == ArmPagingLevel::k3) {
    EXPECT_FALSE(entry.IsTable());
    EXPECT_TRUE(entry.IsPage());
    EXPECT_FALSE(entry.IsBlock());

    static_cast<void>(entry.AsPage());
  } else {
    EXPECT_TRUE(entry.IsTable());
    EXPECT_FALSE(entry.IsPage());
    EXPECT_FALSE(entry.IsBlock());

    static_cast<void>(entry.AsTable());
  }

  // The block format only represents a block for levels 1 and 2.
  entry.set_reg_value(0).set_format(Format::kBlock);
  if constexpr (Level == ArmPagingLevel::k1 || Level == ArmPagingLevel::k2) {
    EXPECT_FALSE(entry.IsTable());
    EXPECT_FALSE(entry.IsPage());
    EXPECT_TRUE(entry.IsBlock());

    static_cast<void>(entry.AsBlock());
  } else {
    EXPECT_FALSE(entry.IsTable());
    EXPECT_FALSE(entry.IsPage());
    EXPECT_FALSE(entry.IsBlock());
  }
}

TEST(ArmPagingTraitTests, ArmDescriptorTypes) {
  TestArmDescriptorTypes<ArmPagingLevel::k0>();
  TestArmDescriptorTypes<ArmPagingLevel::k1>();
  TestArmDescriptorTypes<ArmPagingLevel::k2>();
  TestArmDescriptorTypes<ArmPagingLevel::k3>();
}

template <ArmPagingLevel Level>
void TestArmGetSetTerminal() {
  ArmTableEntry<Level> entry;

  if constexpr (Level == ArmPagingLevel::k3) {
    entry.set_reg_value(0).SetAsPage();
    EXPECT_TRUE(entry.terminal());

    entry.set_reg_value(0).Set({}, ArmPagingSettings{.present = true, .terminal = true});
    EXPECT_TRUE(entry.terminal());
    EXPECT_TRUE(entry.IsPage());
  }

  if constexpr (Level == ArmPagingLevel::k1 || Level == ArmPagingLevel::k2) {
    entry.set_reg_value(0).SetAsBlock();
    EXPECT_TRUE(entry.terminal());

    entry.set_reg_value(0).Set({}, ArmPagingSettings{.present = true, .terminal = true});
    EXPECT_TRUE(entry.terminal());
    EXPECT_TRUE(entry.IsBlock());
  }

  if constexpr (Level != ArmPagingLevel::k3) {
    entry.set_reg_value(0).SetAsTable();
    EXPECT_FALSE(entry.terminal());

    entry.set_reg_value(0).Set({}, ArmPagingSettings{.present = true, .terminal = false});
    EXPECT_FALSE(entry.terminal());
    EXPECT_TRUE(entry.IsTable());
  }
}

TEST(ArmPagingTraitTests, GetSetTerminal) {
  TestArmGetSetTerminal<ArmPagingLevel::k0>();
  TestArmGetSetTerminal<ArmPagingLevel::k1>();
  TestArmGetSetTerminal<ArmPagingLevel::k2>();
  TestArmGetSetTerminal<ArmPagingLevel::k3>();
}

template <ArmPagingLevel Level>
void TestArmGetSetReadable() {
  ArmTableEntry<Level> entry;

  if constexpr (kArmLevelCanBeNonTerminal<Level>) {
    entry.set_reg_value(0).Set({}, ArmPagingSettings{
                                       .present = true,
                                       .terminal = false,
                                       .access = AccessPermissions{.readable = true},
                                   });
    EXPECT_TRUE(entry.readable());
  }

  if constexpr (kArmLevelCanBeTerminal<Level>) {
    entry.set_reg_value(0).Set({}, ArmPagingSettings{
                                       .present = true,
                                       .terminal = true,
                                       .access = AccessPermissions{.readable = true},
                                   });
    EXPECT_TRUE(entry.readable());
  }
}

TEST(ArmPagingTraitTests, GetSetReadable) {
  TestArmGetSetReadable<ArmPagingLevel::k0>();
  TestArmGetSetReadable<ArmPagingLevel::k1>();
  TestArmGetSetReadable<ArmPagingLevel::k2>();
  TestArmGetSetReadable<ArmPagingLevel::k3>();
}

template <ArmPagingLevel Level>
void TestArmGetSetWritableOrUserAccessible() {
  using ArmAccessPermissions = arch::ArmAddressTranslationAccessPermissions;
  using ArmTableAccessPermissions = arch::ArmAddressTranslationTableAccessPermissions;

  ArmTableEntry<Level> entry;

  if constexpr (kArmLevelCanBeTable<Level>) {
    entry.set_reg_value(0).SetAsTable().set_ap_table(ArmTableAccessPermissions::kNoEffect);
    EXPECT_TRUE(entry.writable());
    EXPECT_TRUE(entry.user_accessible());

    entry.set_reg_value(0).SetAsTable().set_ap_table(ArmTableAccessPermissions::kNoEl0Access);
    EXPECT_TRUE(entry.writable());
    EXPECT_FALSE(entry.user_accessible());

    entry.set_reg_value(0).SetAsTable().set_ap_table(ArmTableAccessPermissions::kNoWriteAccess);
    EXPECT_FALSE(entry.writable());
    EXPECT_TRUE(entry.user_accessible());

    entry.set_reg_value(0).SetAsTable().set_ap_table(
        ArmTableAccessPermissions::kNoWriteOrEl0Access);
    EXPECT_FALSE(entry.writable());
    EXPECT_FALSE(entry.user_accessible());
  }

  if constexpr (kArmLevelCanBePage<Level>) {
    entry.set_reg_value(0).SetAsPage().set_ap(ArmAccessPermissions::kSupervisorReadWrite);
    EXPECT_TRUE(entry.writable());
    EXPECT_FALSE(entry.user_accessible());

    entry.set_reg_value(0).SetAsPage().set_ap(ArmAccessPermissions::kReadWrite);
    EXPECT_TRUE(entry.writable());
    EXPECT_TRUE(entry.user_accessible());

    entry.set_reg_value(0).SetAsPage().set_ap(ArmAccessPermissions::kSupervisorReadOnly);
    EXPECT_FALSE(entry.writable());
    EXPECT_FALSE(entry.user_accessible());

    entry.set_reg_value(0).SetAsPage().set_ap(ArmAccessPermissions::kReadOnly);
    EXPECT_FALSE(entry.writable());
    EXPECT_TRUE(entry.user_accessible());
  }

  if constexpr (kArmLevelCanBeBlock<Level>) {
    entry.set_reg_value(0).SetAsBlock().set_ap(ArmAccessPermissions::kSupervisorReadWrite);
    EXPECT_TRUE(entry.writable());
    EXPECT_FALSE(entry.user_accessible());

    entry.set_reg_value(0).SetAsBlock().set_ap(ArmAccessPermissions::kReadWrite);
    EXPECT_TRUE(entry.writable());
    EXPECT_TRUE(entry.user_accessible());

    entry.set_reg_value(0).SetAsBlock().set_ap(ArmAccessPermissions::kSupervisorReadOnly);
    EXPECT_FALSE(entry.writable());
    EXPECT_FALSE(entry.user_accessible());

    entry.set_reg_value(0).SetAsBlock().set_ap(ArmAccessPermissions::kReadOnly);
    EXPECT_FALSE(entry.writable());
    EXPECT_TRUE(entry.user_accessible());
  }

  if constexpr (kArmLevelCanBeNonTerminal<Level>) {
    entry.set_reg_value(0).Set({}, ArmPagingSettings{
                                       .present = true,
                                       .terminal = false,
                                       .access =
                                           AccessPermissions{
                                               .readable = true,
                                               .writable = false,
                                               .user_accessible = false,
                                           },
                                   });
    EXPECT_FALSE(entry.writable());
    EXPECT_FALSE(entry.user_accessible());
    EXPECT_EQ(entry.AsTable().ap_table(), ArmTableAccessPermissions::kNoWriteOrEl0Access);

    entry.set_reg_value(0).Set({}, ArmPagingSettings{
                                       .present = true,
                                       .terminal = false,
                                       .access =
                                           AccessPermissions{
                                               .readable = true,
                                               .writable = false,
                                               .user_accessible = true,
                                           },
                                   });
    EXPECT_FALSE(entry.writable());
    EXPECT_TRUE(entry.user_accessible());
    EXPECT_EQ(entry.AsTable().ap_table(), ArmTableAccessPermissions::kNoWriteAccess);

    entry.set_reg_value(0).Set({}, ArmPagingSettings{
                                       .present = true,
                                       .terminal = false,
                                       .access =
                                           AccessPermissions{
                                               .readable = true,
                                               .writable = true,
                                               .user_accessible = false,
                                           },
                                   });
    EXPECT_TRUE(entry.writable());
    EXPECT_FALSE(entry.user_accessible());
    EXPECT_EQ(entry.AsTable().ap_table(), ArmTableAccessPermissions::kNoEl0Access);

    entry.set_reg_value(0).Set({}, ArmPagingSettings{
                                       .present = true,
                                       .terminal = false,
                                       .access =
                                           AccessPermissions{
                                               .readable = true,
                                               .writable = true,
                                               .user_accessible = true,
                                           },
                                   });
    EXPECT_TRUE(entry.writable());
    EXPECT_TRUE(entry.user_accessible());
    EXPECT_EQ(entry.AsTable().ap_table(), ArmTableAccessPermissions::kNoEffect);
  }

  if constexpr (kArmLevelCanBeTerminal<Level>) {
    auto get_ap = [](auto& entry) {
      if (entry.IsPage()) {
        return entry.AsPage().ap();
      }
      return entry.AsBlock().ap();
    };

    entry.set_reg_value(0).Set({}, ArmPagingSettings{
                                       .present = true,
                                       .terminal = true,
                                       .access =
                                           AccessPermissions{
                                               .readable = true,
                                               .writable = false,
                                               .user_accessible = false,
                                           },
                                   });
    EXPECT_FALSE(entry.writable());
    EXPECT_FALSE(entry.user_accessible());
    EXPECT_EQ(get_ap(entry), ArmAccessPermissions::kSupervisorReadOnly);

    entry.set_reg_value(0).Set({}, ArmPagingSettings{
                                       .present = true,
                                       .terminal = true,
                                       .access =
                                           AccessPermissions{
                                               .readable = true,
                                               .writable = false,
                                               .user_accessible = true,
                                           },
                                   });
    EXPECT_FALSE(entry.writable());
    EXPECT_TRUE(entry.user_accessible());
    EXPECT_EQ(get_ap(entry), ArmAccessPermissions::kReadOnly);

    entry.set_reg_value(0).Set({}, ArmPagingSettings{
                                       .present = true,
                                       .terminal = true,
                                       .access =
                                           AccessPermissions{
                                               .readable = true,
                                               .writable = true,
                                               .user_accessible = false,
                                           },
                                   });
    EXPECT_TRUE(entry.writable());
    EXPECT_FALSE(entry.user_accessible());
    EXPECT_EQ(get_ap(entry), ArmAccessPermissions::kSupervisorReadWrite);

    entry.set_reg_value(0).Set({}, ArmPagingSettings{
                                       .present = true,
                                       .terminal = true,
                                       .access =
                                           AccessPermissions{
                                               .readable = true,
                                               .writable = true,
                                               .user_accessible = true,
                                           },
                                   });
    EXPECT_TRUE(entry.writable());
    EXPECT_TRUE(entry.user_accessible());
    EXPECT_EQ(get_ap(entry), ArmAccessPermissions::kReadWrite);
  }
}

TEST(ArmPagingTraitTests, GetSetWritableOrUserAccessible) {
  TestArmGetSetWritableOrUserAccessible<ArmPagingLevel::k0>();
  TestArmGetSetWritableOrUserAccessible<ArmPagingLevel::k1>();
  TestArmGetSetWritableOrUserAccessible<ArmPagingLevel::k2>();
  TestArmGetSetWritableOrUserAccessible<ArmPagingLevel::k3>();
}

template <ArmPagingLevel Level>
void TestArmGetSetExecutable() {
  ArmTableEntry<Level> entry;

  //
  // `executable()` gives supervisor executability and so should be
  // independent of UXN/UXN_TABLE.
  //

  if constexpr (kArmLevelCanBeTable<Level>) {
    for (bool u : kBools) {
      entry.set_reg_value(0).SetAsTable().set_pxn_table(false).set_uxn_table(u);
      EXPECT_TRUE(entry.executable());

      entry.set_reg_value(0).SetAsTable().set_pxn_table(true).set_uxn_table(u);
      EXPECT_FALSE(entry.executable());
    }
  }

  if constexpr (kArmLevelCanBePage<Level>) {
    for (bool u : kBools) {
      entry.set_reg_value(0).SetAsPage().set_pxn(false).set_uxn(u);
      EXPECT_TRUE(entry.executable());

      entry.set_reg_value(0).SetAsPage().set_pxn(true).set_uxn(u);
      EXPECT_FALSE(entry.executable());
    }
  }

  if constexpr (kArmLevelCanBeBlock<Level>) {
    for (bool u : kBools) {
      entry.set_reg_value(0).SetAsBlock().set_pxn(false).set_uxn(u);
      EXPECT_TRUE(entry.executable());

      entry.set_reg_value(0).SetAsBlock().set_pxn(true).set_uxn(u);
      EXPECT_FALSE(entry.executable());
    }
  }

  //
  // Post-Set() values of UXN/UXN_TABLE should be 1, as user-executable pages
  // are not yet supported.
  //

  if constexpr (kArmLevelCanBeNonTerminal<Level>) {
    for (bool u : kBools) {
      entry.set_reg_value(0).Set({}, ArmPagingSettings{
                                         .present = true,
                                         .terminal = false,
                                         .access =
                                             AccessPermissions{
                                                 .readable = true,
                                                 .executable = false,
                                                 .user_accessible = u,
                                             },
                                     });
      EXPECT_FALSE(entry.executable());
      EXPECT_TRUE(entry.AsTable().pxn_table());
      EXPECT_TRUE(entry.AsTable().uxn_table());

      entry.set_reg_value(0).Set({}, ArmPagingSettings{
                                         .present = true,
                                         .terminal = false,
                                         .access =
                                             AccessPermissions{
                                                 .readable = true,
                                                 .executable = true,
                                                 .user_accessible = u,
                                             },
                                     });
      EXPECT_TRUE(entry.executable());
      EXPECT_FALSE(entry.AsTable().pxn_table());
      EXPECT_TRUE(entry.AsTable().uxn_table());
    }
  }

  if constexpr (kArmLevelCanBeTerminal<Level>) {
    auto get_pxn = [](auto& entry) {
      if (entry.IsPage()) {
        return entry.AsPage().pxn();
      }
      return entry.AsBlock().pxn();
    };
    auto get_uxn = [](auto& entry) {
      if (entry.IsPage()) {
        return entry.AsPage().uxn();
      }
      return entry.AsBlock().uxn();
    };

    for (bool u : kBools) {
      entry.set_reg_value(0).Set({}, ArmPagingSettings{
                                         .present = true,
                                         .terminal = true,
                                         .access =
                                             AccessPermissions{
                                                 .readable = true,
                                                 .executable = false,
                                                 .user_accessible = u,
                                             },
                                     });
      EXPECT_FALSE(entry.executable());
      EXPECT_TRUE(get_pxn(entry));
      EXPECT_TRUE(get_uxn(entry));

      entry.set_reg_value(0).Set({}, ArmPagingSettings{
                                         .present = true,
                                         .terminal = true,
                                         .access =
                                             AccessPermissions{
                                                 .readable = true,
                                                 .executable = true,
                                                 .user_accessible = u,
                                             },
                                     });
      EXPECT_TRUE(entry.executable());
      EXPECT_FALSE(get_pxn(entry));
      EXPECT_TRUE(get_uxn(entry));
    }
  }
}

TEST(ArmPagingTraitTests, GetSetExecutable) {
  TestArmGetSetExecutable<ArmPagingLevel::k0>();
  TestArmGetSetExecutable<ArmPagingLevel::k1>();
  TestArmGetSetExecutable<ArmPagingLevel::k2>();
  TestArmGetSetExecutable<ArmPagingLevel::k3>();
}

template <ArmPagingLevel Level>
void TestArmGetSetAddress() {
  ArmTableEntry<Level> entry;

  constexpr std::array<uint64_t, 4> kAddrs = {
      0x0000'0000'0000'1000u,  // 4KiB-aligned
      0x0000'0000'0020'0000u,  // 2MiB-aligned
      0x0000'0000'4000'0000u,  // 1GiB-aligned
      0x0000'0080'0000'0000u,  // 512GiB-aligned
  };

  if constexpr (kArmLevelCanBeNonTerminal<Level>) {
    for (size_t i = 0; i < kAddrs.size(); ++i) {
      uint64_t addr = kAddrs[i];

      entry.set_reg_value(0).SetAsTable().set_table_address(addr);
      EXPECT_EQ(addr, entry.address());

      entry.set_reg_value(0).Set({}, ArmPagingSettings{
                                         .address = addr,
                                         .present = true,
                                         .terminal = false,
                                     });
      EXPECT_EQ(addr, entry.address());
      EXPECT_EQ(addr, entry.AsTable().table_address());
    }
  }

  if constexpr (kArmLevelCanBeTerminal<Level>) {
    constexpr size_t kFirstValid = []() {
      switch (Level) {
        case ArmPagingLevel::k1:
          return 2;
        case ArmPagingLevel::k2:
          return 1;
        case ArmPagingLevel::k3:
          return 0;
      }
    }();
    for (size_t i = kFirstValid; i < kAddrs.size(); ++i) {
      uint64_t addr = kAddrs[i];

      if constexpr (Level == ArmPagingLevel::k3) {
        entry.set_reg_value(0).SetAsPage().set_output_address(addr);
      } else {
        entry.set_reg_value(0).SetAsBlock().set_output_address(addr);
      }
      EXPECT_EQ(addr, entry.address());

      entry.set_reg_value(0).Set({}, ArmPagingSettings{
                                         .address = addr,
                                         .present = true,
                                         .terminal = true,
                                     });
      EXPECT_EQ(addr, entry.address());
      if constexpr (Level == ArmPagingLevel::k3) {
        EXPECT_EQ(addr, entry.AsPage().output_address());
      } else {
        EXPECT_EQ(addr, entry.AsBlock().output_address());
      }
    }
  }
}

TEST(ArmPagingTraitTests, GetSetAddress) {
  TestArmGetSetAddress<ArmPagingLevel::k0>();
  TestArmGetSetAddress<ArmPagingLevel::k1>();
  TestArmGetSetAddress<ArmPagingLevel::k2>();
  TestArmGetSetAddress<ArmPagingLevel::k3>();
}

template <ArmPagingLevel Level>
void TestArmGetSetMemory() {
  constexpr arch::ArmMairNormalAttribute kNormalMemory = {
      .inner = arch::ArmCacheabilityAttribute::kWriteBackReadWriteAllocate,
      .outer = arch::ArmCacheabilityAttribute::kWriteBackReadWriteAllocate,
  };

  constexpr arch::ArmMairAttribute kMmioMemory =
      arch::ArmDeviceMemory::kNonGatheringNonReorderingEarlyAck;

  const ArmSystemState kState = {
      .mair = arch::ArmMemoryAttrIndirectionRegister::Get()
                  .FromValue(0)
                  .SetAttribute(0, kNormalMemory)
                  .SetAttribute(1, kMmioMemory),
      .shareability = arch::ArmShareabilityAttribute::kOuter,
  };

  ArmTableEntry<Level> entry;

  if constexpr (kArmLevelCanBePage<Level>) {
    entry.set_reg_value(0).SetAsPage().set_attr_index(0);
    EXPECT_EQ(kNormalMemory, entry.Memory(kState));

    entry.set_reg_value(0).SetAsPage().set_attr_index(1);
    EXPECT_EQ(kMmioMemory, entry.Memory(kState));
  } else if constexpr (kArmLevelCanBeBlock<Level>) {
    entry.set_reg_value(0).SetAsBlock().set_attr_index(0);
    EXPECT_EQ(kNormalMemory, entry.Memory(kState));

    entry.set_reg_value(0).SetAsBlock().set_attr_index(1);
    EXPECT_EQ(kMmioMemory, entry.Memory(kState));
  }

  if constexpr (kArmLevelCanBeTerminal<Level>) {
    auto attr_index = [](auto& entry) {
      if (entry.IsPage()) {
        return entry.AsPage().attr_index();
      }
      return entry.AsBlock().attr_index();
    };

    entry.set_reg_value(0).Set(kState, ArmPagingSettings{
                                           .present = true,
                                           .terminal = true,
                                           .access = {.readable = true},
                                           .memory = kNormalMemory,
                                       });
    EXPECT_EQ(0u, attr_index(entry));
    EXPECT_EQ(kNormalMemory, entry.Memory(kState));

    entry.set_reg_value(0).Set(kState, ArmPagingSettings{
                                           .present = true,
                                           .terminal = true,
                                           .access = {.readable = true},
                                           .memory = kMmioMemory,
                                       });
    EXPECT_EQ(1u, attr_index(entry));
    EXPECT_EQ(kMmioMemory, entry.Memory(kState));
  }
}

TEST(ArmPagingTraitTests, GetSetMemory) {
  TestArmGetSetMemory<ArmPagingLevel::k0>();
  TestArmGetSetMemory<ArmPagingLevel::k1>();
  TestArmGetSetMemory<ArmPagingLevel::k2>();
  TestArmGetSetMemory<ArmPagingLevel::k3>();
}

template <ArmPagingLevel Level>
void TestArmGetSetAccessed() {
  ArmTableEntry<Level> entry;

  if constexpr (kArmLevelCanBeTable<Level>) {
    entry.set_reg_value(0).SetAsTable();
    EXPECT_FALSE(entry.accessed());
  }
  if constexpr (kArmLevelCanBePage<Level>) {
    entry.set_reg_value(0).SetAsPage().set_af(0);
    EXPECT_FALSE(entry.accessed());

    entry.set_reg_value(0).SetAsPage().set_af(1);
    EXPECT_TRUE(entry.accessed());
  }
  if constexpr (kArmLevelCanBeBlock<Level>) {
    entry.set_reg_value(0).SetAsBlock().set_af(0);
    EXPECT_FALSE(entry.accessed());

    entry.set_reg_value(0).SetAsBlock().set_af(1);
    EXPECT_TRUE(entry.accessed());
  }

  // Non-terminal entries never report 'accessed'.
  if constexpr (kArmLevelCanBeNonTerminal<Level>) {
    entry.set_reg_value(0).Set({}, ArmPagingSettings{
                                       .present = true,
                                       .terminal = false,
                                       .access = {.readable = true},
                                       .accessed = false,
                                   });
    EXPECT_FALSE(entry.accessed());

    entry.set_reg_value(0).Set({}, ArmPagingSettings{
                                       .present = true,
                                       .terminal = false,
                                       .access = {.readable = true},
                                       .accessed = true,
                                   });
    EXPECT_FALSE(entry.accessed());
  }

  if constexpr (kArmLevelCanBeTerminal<Level>) {
    entry.set_reg_value(0).Set({}, ArmPagingSettings{
                                       .present = true,
                                       .terminal = true,
                                       .access = {.readable = true},
                                       .accessed = false,
                                   });
    EXPECT_FALSE(entry.accessed());

    entry.set_reg_value(0).Set({}, ArmPagingSettings{
                                       .present = true,
                                       .terminal = true,
                                       .access = {.readable = true},
                                       .accessed = true,
                                   });
    EXPECT_TRUE(entry.accessed());
  }
}

TEST(ArmPagingTraitTests, GetSetAccessed) {
  TestArmGetSetAccessed<ArmPagingLevel::k0>();
  TestArmGetSetAccessed<ArmPagingLevel::k1>();
  TestArmGetSetAccessed<ArmPagingLevel::k2>();
  TestArmGetSetAccessed<ArmPagingLevel::k3>();
}

// Represents a 512 entry page table and owns its own storage.
class Table {
 public:
  using Io = hwreg::ArrayIo<uint64_t, 512>;

  Io io() { return table_.direct_io(); }

  uint64_t paddr() const { return reinterpret_cast<uint64_t>(&table_); }

  void Set(uint64_t entry, size_t index) { table_.table()[index] = entry; }

 private:
  hwreg::AlignedTableStorage<uint64_t, 512> table_;
};

// A simple helper for managing table allocations and offering the
// paddr-to-I/O-provider abstraction required of paging API.
//
// For simplicity, this class is intended to be used by paging schemes with
// tables of 512 entries.
template <class PagingTraits>
class PagingHelper {
 public:
  using LevelType = typename PagingTraits::LevelType;

  template <LevelType Level>
  using TableEntry = typename PagingTraits::template TableEntry<Level>;

  static constexpr auto kLevels = PagingTraits::kLevels;

  Table& NewTable() {
    std::unique_ptr<Table> table(new Table);
    uint64_t addr = reinterpret_cast<uint64_t>(table.get());
    auto [it, ok] = tables_.insert_or_assign(addr, std::move(table));
    ZX_ASSERT(ok);
    return *it->second;
  }

  auto MakePaddrToIo() {
    return [this](uint64_t paddr) -> Table::Io {
      auto it = tables_.find(paddr);
      if (it == tables_.end()) {
        bool first = true;
        printf("unknown \"physical\" table address %#" PRIx64 "; known table addresses:", paddr);
        for (const auto& [addr, table] : tables_) {
          printf("%s %#" PRIx64, first ? "" : ",", addr);
          first = false;
        }
        abort();
      }
      return it->second->io();
    };
  }

  auto MakeAllocator() {
    return [this](uint64_t size, uint64_t alignment) {
      ZX_ASSERT(size == sizeof(Table));
      ZX_ASSERT(alignment == alignof(Table));
      return NewTable().paddr();
    };
  }

  template <size_t LevelIndex>
  TableEntry<kLevels[LevelIndex]> NewTableEntry() const {
    return TableEntry<kLevels[LevelIndex]>{}.set_reg_value(0);
  }

 private:
  static_assert(PagingTraits::kTableAlignmentLog2 == 12);

  template <size_t... LevelIndex>
  static constexpr bool Has512Entries(std::index_sequence<LevelIndex...>) {
    return ((PagingTraits::template kNumTableEntriesLog2<kLevels[LevelIndex]> == 9) && ...);
  }
  static_assert(Has512Entries(std::make_index_sequence<kLevels.size()>()));

  std::map<uint64_t, std::unique_ptr<Table>> tables_;
};

TEST(PagingTests, Compilation) {
  // The point here is not really to check that the traits structs are empty,
  // but rather to force the compiler to instantiate arch::Paging for each
  // trait, which in turn checks that the traits meet the expected API.
  static_assert(std::is_empty_v<arch::Paging<arch::ExamplePagingTraits>>);
  static_assert(std::is_empty_v<arch::Paging<arch::RiscvSv39PagingTraits>>);
  static_assert(std::is_empty_v<arch::Paging<arch::RiscvSv48PagingTraits>>);
  static_assert(std::is_empty_v<arch::Paging<arch::RiscvSv57PagingTraits>>);
  static_assert(std::is_empty_v<arch::Paging<arch::X86FourLevelPagingTraits>>);
  static_assert(std::is_empty_v<arch::Paging<arch::X86FiveLevelPagingTraits>>);
  static_assert(
      std::is_empty_v<arch::Paging<arch::ArmPagingTraits<arch::ArmVirtualAddressRange::kLower>>>);
  static_assert(
      std::is_empty_v<arch::Paging<arch::ArmPagingTraits<arch::ArmVirtualAddressRange::kUpper>>>);
}

// The following macros are expected to be used on test cases given as
// functions of the form
// ```
// template <class PagingTraits>
// void CaseName(const typename PagingTraits::SystemState& state)
// ```
// stamping out test cases for each PagingTraits implementation under test.

#define TEST_FOR_ALL_TRAITS(name) \
  TEST_FOR_ARM(name)              \
  TEST_FOR_RISCV(name)            \
  TEST_FOR_X86(name)

#define TEST_FOR_ARM(name)                                            \
  TEST(PagingTests, ArmLower##name) {                                 \
    name<arch::ArmPagingTraits<arch::ArmVirtualAddressRange::kLower>, \
         /*VaddrHighBits=*/0x0000'0000'0000'0000u>({});               \
  }                                                                   \
  TEST(PagingTests, ArmUpper##name) {                                 \
    name<arch::ArmPagingTraits<arch::ArmVirtualAddressRange::kUpper>, \
         /*VaddrHighBits=*/0xffff'0000'0000'0000u>({});               \
  }

#define TEST_FOR_RISCV(name) \
  TEST(PagingTests, RiscvSv48##name) { name<arch::RiscvSv48PagingTraits>({}); }

#define TEST_FOR_X86(name)                                    \
  TEST(PagingTests, X86##name##NoGibPages) {                  \
    name<arch::X86FourLevelPagingTraits>({.page1gb = false}); \
  }                                                           \
  TEST(PagingTests, X86##name##GibPages) {                    \
    name<arch::X86FourLevelPagingTraits>({.page1gb = true});  \
  }

template <typename SystemState>
bool OneGibPagesAllowed(const SystemState& state) {
  return true;
}

bool OneGibPagesAllowed(const X86SystemState& state) { return state.page1gb; }

template <class PagingTraits, uint64_t VaddrHighBits = 0>
void TranslationWith4KiBPages(const typename PagingTraits::SystemState& state) {
  using Paging = arch::Paging<PagingTraits>;
  using PagingSettings = arch::PagingSettings<typename PagingTraits::MemoryType>;

  //                                |---9---| |---9---| |---9---| |---9---| |----12----|
  constexpr uint64_t kPageVaddr =
      VaddrHighBits | 0b111111111'101010101'010101010'100100100'000000000000;
  constexpr uint64_t kPagePaddr = 0xffff'ffff'1000;

  PagingHelper<PagingTraits> helper;
  Table& first = helper.NewTable();
  Table& second = helper.NewTable();
  Table& third = helper.NewTable();
  Table& fourth = helper.NewTable();

  {
    auto entry = helper.template NewTableEntry<0>().Set(state, PagingSettings{
                                                                   .address = second.paddr(),
                                                                   .present = true,
                                                                   .terminal = false,
                                                                   .access = kRWXU,
                                                               });
    first.Set(entry.reg_value(), 0b111111111);
  }
  {
    auto entry = helper.template NewTableEntry<1>().Set(state, PagingSettings{
                                                                   .address = third.paddr(),
                                                                   .present = true,
                                                                   .terminal = false,
                                                                   .access = kRWXU,
                                                               });
    second.Set(entry.reg_value(), 0b101010101);
  }
  {
    auto entry = helper.template NewTableEntry<2>().Set(state, PagingSettings{
                                                                   .address = fourth.paddr(),
                                                                   .present = true,
                                                                   .terminal = false,
                                                                   .access = kRWXU,
                                                               });
    third.Set(entry.reg_value(), 0b010101010);
  }
  {
    auto entry = helper.template NewTableEntry<3>().Set(state, PagingSettings{
                                                                   .address = kPagePaddr,
                                                                   .present = true,
                                                                   .terminal = true,
                                                                   .access = kRWXU,
                                                               });
    fourth.Set(entry.reg_value(), 0b100100100);
  }

  {
    auto result = Paging::template Query(first.paddr(), helper.MakePaddrToIo(), kPageVaddr);
    ASSERT_TRUE(result.is_ok());

    auto [paddr, page, access] = std::move(result).value();
    EXPECT_EQ(kPagePaddr, paddr);
    EXPECT_EQ(kPagePaddr, page.paddr);
    EXPECT_EQ(0x1000u, page.size);
    EXPECT_TRUE(access.readable);
    EXPECT_TRUE(access.writable);
    EXPECT_TRUE(access.executable);
    EXPECT_TRUE(access.user_accessible);
  }
  {
    auto result = Paging::template Query(first.paddr(), helper.MakePaddrToIo(), kPageVaddr | 0xabc);
    ASSERT_TRUE(result.is_ok());

    auto [paddr, page, access] = std::move(result).value();
    EXPECT_EQ(kPagePaddr | 0xabc, paddr);
    EXPECT_EQ(kPagePaddr, page.paddr);
    EXPECT_EQ(0x1000u, page.size);
    EXPECT_TRUE(access.readable);
    EXPECT_TRUE(access.writable);
    EXPECT_TRUE(access.executable);
    EXPECT_TRUE(access.user_accessible);
  }
  {
    auto result = Paging::template Query(first.paddr(), helper.MakePaddrToIo(), kPageVaddr | 0xfff);
    ASSERT_TRUE(result.is_ok());

    auto [paddr, page, access] = std::move(result).value();
    EXPECT_EQ(kPagePaddr | 0xfff, paddr);
    EXPECT_EQ(kPagePaddr, page.paddr);
    EXPECT_EQ(0x1000u, page.size);
    EXPECT_TRUE(access.readable);
    EXPECT_TRUE(access.writable);
    EXPECT_TRUE(access.executable);
    EXPECT_TRUE(access.user_accessible);
  }
}
TEST_FOR_ALL_TRAITS(TranslationWith4KiBPages)

template <class PagingTraits, uint64_t VaddrHighBits = 0>
void TranslationWith2MiBPages(const typename PagingTraits::SystemState& state) {
  using Paging = arch::Paging<PagingTraits>;
  using PagingSettings = arch::PagingSettings<typename PagingTraits::MemoryType>;

  //                                |---9---| |---9---| |---9---| |------12 + 9-------|
  constexpr uint64_t kPageVaddr =
      VaddrHighBits | 0b111111111'101010101'010101010'000000000000000000000;
  constexpr uint64_t kPagePaddr = 0xffff'ff20'0000;

  PagingHelper<PagingTraits> helper;
  Table& first = helper.NewTable();
  Table& second = helper.NewTable();
  Table& third = helper.NewTable();

  {
    auto entry = helper.template NewTableEntry<0>().Set(state, PagingSettings{
                                                                   .address = second.paddr(),
                                                                   .present = true,
                                                                   .terminal = false,
                                                                   .access = kRWXU,
                                                               });
    first.Set(entry.reg_value(), 0b111111111);
  }
  {
    auto entry = helper.template NewTableEntry<1>().Set(state, PagingSettings{
                                                                   .address = third.paddr(),
                                                                   .present = true,
                                                                   .terminal = false,
                                                                   .access = kRWXU,
                                                               });
    second.Set(entry.reg_value(), 0b101010101);
  }
  {
    auto entry = helper.template NewTableEntry<2>().Set(state, PagingSettings{
                                                                   .address = kPagePaddr,
                                                                   .present = true,
                                                                   .terminal = true,
                                                                   .access = kRWXU,
                                                               });
    third.Set(entry.reg_value(), 0b010101010);
  }

  {
    auto result = Paging::template Query(first.paddr(), helper.MakePaddrToIo(), kPageVaddr);
    ASSERT_TRUE(result.is_ok());

    auto [paddr, page, access] = std::move(result).value();
    EXPECT_EQ(kPagePaddr, paddr);
    EXPECT_EQ(kPagePaddr, page.paddr);
    EXPECT_EQ(0x20'0000u, page.size);
    EXPECT_TRUE(access.readable);
    EXPECT_TRUE(access.writable);
    EXPECT_TRUE(access.executable);
    EXPECT_TRUE(access.user_accessible);
  }
  {
    auto result =
        Paging::template Query(first.paddr(), helper.MakePaddrToIo(), kPageVaddr | 0xabcde);
    ASSERT_TRUE(result.is_ok());

    auto [paddr, page, access] = std::move(result).value();
    EXPECT_EQ(kPagePaddr | 0xabcde, paddr);
    EXPECT_EQ(kPagePaddr, page.paddr);
    EXPECT_EQ(0x20'0000u, page.size);
    EXPECT_TRUE(access.readable);
    EXPECT_TRUE(access.writable);
    EXPECT_TRUE(access.executable);
    EXPECT_TRUE(access.user_accessible);
  }
  {
    auto result =
        Paging::template Query(first.paddr(), helper.MakePaddrToIo(), kPageVaddr | 0x1f'ffff);
    ASSERT_TRUE(result.is_ok());

    auto [paddr, page, access] = std::move(result).value();
    EXPECT_EQ(kPagePaddr | 0x1f'ffff, paddr);
    EXPECT_EQ(kPagePaddr, page.paddr);
    EXPECT_EQ(0x20'0000u, page.size);
    EXPECT_TRUE(access.readable);
    EXPECT_TRUE(access.writable);
    EXPECT_TRUE(access.executable);
    EXPECT_TRUE(access.user_accessible);
  }
}
TEST_FOR_ALL_TRAITS(TranslationWith2MiBPages)

template <class PagingTraits, uint64_t VaddrHighBits = 0>
void TranslationWith1GiBPages(const typename PagingTraits::SystemState& state) {
  using Paging = arch::Paging<PagingTraits>;
  using PagingSettings = arch::PagingSettings<typename PagingTraits::MemoryType>;

  //                                |---9---| |---9---| |---------12 + 9 + 9---------|
  constexpr uint64_t kPageVaddr =
      VaddrHighBits | 0b111111111'101010101'000000000000000000000000000000;
  constexpr uint64_t kPagePaddr = 0xffff'4000'0000;

  PagingHelper<PagingTraits> helper;
  Table& first = helper.NewTable();
  Table& second = helper.NewTable();

  {
    auto entry = helper.template NewTableEntry<0>().Set(state, PagingSettings{
                                                                   .address = second.paddr(),
                                                                   .present = true,
                                                                   .terminal = false,
                                                                   .access = kRWXU,
                                                               });
    first.Set(entry.reg_value(), 0b111111111);
  }
  {
    auto entry = helper.template NewTableEntry<1>().Set(state, PagingSettings{
                                                                   .address = kPagePaddr,
                                                                   .present = true,
                                                                   .terminal = true,
                                                                   .access = kRWXU,
                                                               });
    second.Set(entry.reg_value(), 0b101010101);
  }

  {
    auto result = Paging::template Query(first.paddr(), helper.MakePaddrToIo(), kPageVaddr);
    ASSERT_TRUE(result.is_ok());

    auto [paddr, page, access] = std::move(result).value();
    EXPECT_EQ(kPagePaddr, paddr);
    EXPECT_EQ(kPagePaddr, page.paddr);
    EXPECT_EQ(0x4000'0000u, page.size);
    EXPECT_TRUE(access.readable);
    EXPECT_TRUE(access.writable);
    EXPECT_TRUE(access.executable);
    EXPECT_TRUE(access.user_accessible);
  }
  {
    auto result =
        Paging::template Query(first.paddr(), helper.MakePaddrToIo(), kPageVaddr | 0xabc'def0);
    ASSERT_TRUE(result.is_ok());

    auto [paddr, page, access] = std::move(result).value();
    EXPECT_EQ(kPagePaddr | 0xabc'def0, paddr);
    EXPECT_EQ(kPagePaddr, page.paddr);
    EXPECT_EQ(0x4000'0000u, page.size);
    EXPECT_TRUE(access.readable);
    EXPECT_TRUE(access.writable);
    EXPECT_TRUE(access.executable);
    EXPECT_TRUE(access.user_accessible);
  }
  {
    auto result =
        Paging::template Query(first.paddr(), helper.MakePaddrToIo(), kPageVaddr | 0x3fff'ffff);
    ASSERT_TRUE(result.is_ok());

    auto [paddr, page, access] = std::move(result).value();
    EXPECT_EQ(kPagePaddr | 0x3fff'ffff, paddr);
    EXPECT_EQ(kPagePaddr, page.paddr);
    EXPECT_EQ(0x4000'0000u, page.size);
    EXPECT_TRUE(access.readable);
    EXPECT_TRUE(access.writable);
    EXPECT_TRUE(access.executable);
    EXPECT_TRUE(access.user_accessible);
  }
}
TEST_FOR_ALL_TRAITS(TranslationWith1GiBPages)

template <class PagingTraits, uint64_t VaddrHighBits = 0>
void TranslationFault(const typename PagingTraits::SystemState& state) {
  using Paging = arch::Paging<PagingTraits>;
  using PagingSettings = arch::PagingSettings<typename PagingTraits::MemoryType>;

  //                                |---9---| |---9---| |---9---| |---9---| |----12----|
  constexpr uint64_t kPageVaddr =
      VaddrHighBits | 0b111111111'101010101'010101010'100100100'000000000000;
  constexpr uint64_t kPagePaddr = 0xffff'ffff'1000;

  PagingHelper<PagingTraits> helper;
  Table& first = helper.NewTable();
  Table& second = helper.NewTable();
  Table& third = helper.NewTable();
  Table& fourth = helper.NewTable();

  // Zero out the entries we expect a successful translation of kPageVaddr to
  // walk.
  first.Set(0, 0b111111111);
  second.Set(0, 0b101010101);
  third.Set(0, 0b010101010);
  fourth.Set(0, 0b100100100);

  //
  // We should fault... and continue faulting till we properly fill out these
  // entries to point to kPagePaddr.
  //

  {
    auto result = Paging::template Query(first.paddr(), helper.MakePaddrToIo(), kPageVaddr);
    EXPECT_TRUE(result.is_error());
  }

  {
    auto entry = helper.template NewTableEntry<0>().Set(state, PagingSettings{
                                                                   .address = second.paddr(),
                                                                   .present = true,
                                                                   .terminal = false,
                                                                   .access = kRWXU,
                                                               });
    first.Set(entry.reg_value(), 0b111111111);
  }

  {
    auto result = Paging::template Query(first.paddr(), helper.MakePaddrToIo(), kPageVaddr);
    EXPECT_TRUE(result.is_error());
  }

  {
    auto entry = helper.template NewTableEntry<1>().Set(state, PagingSettings{
                                                                   .address = third.paddr(),
                                                                   .present = true,
                                                                   .terminal = false,
                                                                   .access = kRWXU,
                                                               });
    second.Set(entry.reg_value(), 0b101010101);
  }

  {
    auto result = Paging::template Query(first.paddr(), helper.MakePaddrToIo(), kPageVaddr);
    EXPECT_TRUE(result.is_error());
  }

  {
    auto entry = helper.template NewTableEntry<2>().Set(state, PagingSettings{
                                                                   .address = fourth.paddr(),
                                                                   .present = true,
                                                                   .terminal = false,
                                                                   .access = kRWXU,
                                                               });
    third.Set(entry.reg_value(), 0b010101010);
  }

  {
    auto result = Paging::template Query(first.paddr(), helper.MakePaddrToIo(), kPageVaddr);
    EXPECT_TRUE(result.is_error());
  }

  {
    auto entry = helper.template NewTableEntry<3>().Set(state, PagingSettings{
                                                                   .address = kPagePaddr,
                                                                   .present = true,
                                                                   .terminal = true,
                                                                   .access = kRWXU,
                                                               });
    fourth.Set(entry.reg_value(), 0b100100100);
  }

  // All entries are filled in and point to kPagePaddr, so we should no longer
  // see a fault.
  {
    auto result = Paging::template Query(first.paddr(), helper.MakePaddrToIo(), kPageVaddr);
    ASSERT_TRUE(result.is_ok());
    EXPECT_EQ(kPagePaddr, result->paddr);
  }
}

TEST_FOR_ALL_TRAITS(TranslationFault)

template <class PagingTraits, uint64_t VaddrHighBits = 0>
void Mapped4KiBPage(typename PagingTraits::SystemState state) {
  using Paging = arch::Paging<PagingTraits>;
  using MapSettings = arch::MapSettings<typename PagingTraits::MemoryType>;

  //                                |---9---| |---9---| |---9---| |---9---| |----12----|
  constexpr uint64_t kPageVaddr =
      VaddrHighBits | 0b111111111'101010101'010101010'100100100'000000000000;
  constexpr uint64_t kPagePaddr = 0xffff'ffff'1000;

  constexpr MapSettings kSettings = {
      .access =
          AccessPermissions{
              .readable = true,
              .writable = true,
              .executable = true,
              .user_accessible = true,
          },
  };

  PagingHelper<PagingTraits> helper;
  Table& root = helper.NewTable();

  {
    auto result = Paging::template Map(root.paddr(), helper.MakePaddrToIo(), helper.MakeAllocator(),
                                       state, kPageVaddr, 0x1000, kPagePaddr, kSettings);
    ASSERT_TRUE(result.is_ok());
  }

  // We can test Map() against the previously-tested Query()
  {
    auto result = Paging::template Query(root.paddr(), helper.MakePaddrToIo(), kPageVaddr);
    ASSERT_TRUE(result.is_ok());

    auto [paddr, page, access] = std::move(result).value();

    EXPECT_EQ(kPagePaddr, paddr);

    EXPECT_EQ(kPagePaddr, page.paddr);
    EXPECT_EQ(0x1000u, page.size);

    EXPECT_TRUE(access.readable);
    EXPECT_TRUE(access.writable);
    EXPECT_TRUE(access.executable);
    EXPECT_TRUE(access.user_accessible);
  }
  {
    auto result = Paging::template Query(root.paddr(), helper.MakePaddrToIo(), kPageVaddr | 0xabc);
    ASSERT_TRUE(result.is_ok());

    auto [paddr, page, access] = std::move(result).value();

    EXPECT_EQ(kPagePaddr | 0xabc, paddr);

    EXPECT_EQ(kPagePaddr, page.paddr);
    EXPECT_EQ(0x1000u, page.size);

    EXPECT_TRUE(access.readable);
    EXPECT_TRUE(access.writable);
    EXPECT_TRUE(access.executable);
    EXPECT_TRUE(access.user_accessible);
  }
  {
    auto result = Paging::template Query(root.paddr(), helper.MakePaddrToIo(), kPageVaddr | 0xfff);
    ASSERT_TRUE(result.is_ok());

    auto [paddr, page, access] = std::move(result).value();

    EXPECT_EQ(kPagePaddr | 0xfff, paddr);

    EXPECT_EQ(kPagePaddr, page.paddr);
    EXPECT_EQ(0x1000u, page.size);

    EXPECT_TRUE(access.readable);
    EXPECT_TRUE(access.writable);
    EXPECT_TRUE(access.executable);
    EXPECT_TRUE(access.user_accessible);
  }
}
TEST_FOR_ALL_TRAITS(Mapped4KiBPage)

template <class PagingTraits, uint64_t VaddrHighBits = 0>
void Mapped2MiBPage(typename PagingTraits::SystemState state) {
  using Paging = arch::Paging<PagingTraits>;
  using MapSettings = arch::MapSettings<typename PagingTraits::MemoryType>;

  //                                |---9---| |---9---| |---9---| |------12 + 9-------|
  constexpr uint64_t kPageVaddr =
      VaddrHighBits | 0b111111111'101010101'010101010'000000000000000000000;
  constexpr uint64_t kPagePaddr = 0xffff'ff20'0000;

  constexpr MapSettings kSettings = {
      .access =
          {
              .readable = true,
              .writable = true,
              .executable = true,
              .user_accessible = true,
          },
  };

  PagingHelper<PagingTraits> helper;
  Table& root = helper.NewTable();

  {
    auto result = Paging::template Map(root.paddr(), helper.MakePaddrToIo(), helper.MakeAllocator(),
                                       state, kPageVaddr, 0x20'0000, kPagePaddr, kSettings);
    ASSERT_TRUE(result.is_ok());
  }

  // We can test Map() against the previously-tested Query()
  {
    auto result = Paging::template Query(root.paddr(), helper.MakePaddrToIo(), kPageVaddr);
    ASSERT_TRUE(result.is_ok());

    auto [paddr, page, access] = std::move(result).value();

    EXPECT_EQ(kPagePaddr, paddr);

    EXPECT_EQ(kPagePaddr, page.paddr);
    EXPECT_EQ(0x20'0000u, page.size);

    EXPECT_TRUE(access.readable);
    EXPECT_TRUE(access.writable);
    EXPECT_TRUE(access.executable);
    EXPECT_TRUE(access.user_accessible);
  }
  {
    auto result =
        Paging::template Query(root.paddr(), helper.MakePaddrToIo(), kPageVaddr | 0xa'bcde);
    ASSERT_TRUE(result.is_ok());

    auto [paddr, page, access] = std::move(result).value();

    EXPECT_EQ(kPagePaddr | 0xa'bcde, paddr);

    EXPECT_EQ(kPagePaddr, page.paddr);
    EXPECT_EQ(0x20'0000u, page.size);

    EXPECT_TRUE(access.readable);
    EXPECT_TRUE(access.writable);
    EXPECT_TRUE(access.executable);
    EXPECT_TRUE(access.user_accessible);
  }
  {
    auto result =
        Paging::template Query(root.paddr(), helper.MakePaddrToIo(), kPageVaddr | 0x1f'ffff);
    ASSERT_TRUE(result.is_ok());

    auto [paddr, page, access] = std::move(result).value();

    EXPECT_EQ(kPagePaddr | 0x1f'ffff, paddr);

    EXPECT_EQ(kPagePaddr, page.paddr);
    EXPECT_EQ(0x20'0000u, page.size);

    EXPECT_TRUE(access.readable);
    EXPECT_TRUE(access.writable);
    EXPECT_TRUE(access.executable);
    EXPECT_TRUE(access.user_accessible);
  }
}
TEST_FOR_ALL_TRAITS(Mapped2MiBPage)

template <class PagingTraits, uint64_t VaddrHighBits = 0>
void Mapped1GiBPage(typename PagingTraits::SystemState state) {
  using Paging = arch::Paging<PagingTraits>;
  using MapSettings = arch::MapSettings<typename PagingTraits::MemoryType>;

  //                                |---9---| |---9---| |---------12 + 9 + 9---------|
  constexpr uint64_t kPageVaddr =
      VaddrHighBits | 0b111111111'101010101'000000000000000000000000000000;
  constexpr uint64_t kPagePaddr = 0xffff'4000'0000;

  constexpr MapSettings kSettings = {
      .access =
          {
              .readable = true,
              .writable = true,
              .executable = true,
              .user_accessible = true,
          },
  };

  PagingHelper<PagingTraits> helper;
  Table& root = helper.NewTable();

  {
    auto result = Paging::template Map(root.paddr(), helper.MakePaddrToIo(), helper.MakeAllocator(),
                                       state, kPageVaddr, 0x4000'0000, kPagePaddr, kSettings);
    ASSERT_TRUE(result.is_ok());
  }

  // We can test Map() against the previously-tested Query()
  {
    auto result = Paging::template Query(root.paddr(), helper.MakePaddrToIo(), kPageVaddr);
    ASSERT_TRUE(result.is_ok());

    auto [paddr, page, access] = std::move(result).value();

    EXPECT_EQ(kPagePaddr, paddr);

    if (OneGibPagesAllowed(state)) {
      EXPECT_EQ(kPagePaddr, page.paddr);
      EXPECT_EQ(0x4000'0000u, page.size);
    } else {
      // 512 2MiB pages otherwise.
      EXPECT_EQ(kPagePaddr, page.paddr);
      EXPECT_EQ(0x20'0000u, page.size);
    }

    EXPECT_TRUE(access.readable);
    EXPECT_TRUE(access.writable);
    EXPECT_TRUE(access.executable);
    EXPECT_TRUE(access.user_accessible);
  }
  {
    auto result =
        Paging::template Query(root.paddr(), helper.MakePaddrToIo(), kPageVaddr | 0xabc'def0);
    ASSERT_TRUE(result.is_ok());

    auto [paddr, page, access] = std::move(result).value();

    EXPECT_EQ(kPagePaddr | 0xabc'def0, paddr);

    if (OneGibPagesAllowed(state)) {
      EXPECT_EQ(kPagePaddr, page.paddr);
      EXPECT_EQ(0x4000'0000u, page.size);
    } else {
      // 512 2MiB pages otherwise.
      EXPECT_EQ(kPagePaddr | 0xaa0'0000, page.paddr);
      EXPECT_EQ(0x20'0000u, page.size);
    }

    EXPECT_TRUE(access.readable);
    EXPECT_TRUE(access.writable);
    EXPECT_TRUE(access.executable);
    EXPECT_TRUE(access.user_accessible);
  }
  {
    auto result =
        Paging::template Query(root.paddr(), helper.MakePaddrToIo(), kPageVaddr | 0x3fff'ffff);
    ASSERT_TRUE(result.is_ok());

    auto [paddr, page, access] = std::move(result).value();

    EXPECT_EQ(kPagePaddr | 0x3fff'ffff, paddr);

    if (OneGibPagesAllowed(state)) {
      EXPECT_EQ(kPagePaddr, page.paddr);
      EXPECT_EQ(0x4000'0000u, page.size);
    } else {
      // 512 2MiB pages otherwise.
      EXPECT_EQ(kPagePaddr | 0x3fe0'0000, page.paddr);
      EXPECT_EQ(0x20'0000u, page.size);
    }

    EXPECT_TRUE(access.readable);
    EXPECT_TRUE(access.writable);
    EXPECT_TRUE(access.executable);
    EXPECT_TRUE(access.user_accessible);
  }
}
TEST_FOR_ALL_TRAITS(Mapped1GiBPage)

template <class PagingTraits, uint64_t VaddrHighBits = 0>
void DoubleMapping(typename PagingTraits::SystemState state) {
  //
  // Performing the same mapping twice should yield kAlreadyMapped.
  //

  using Paging = arch::Paging<PagingTraits>;
  using MapSettings = arch::MapSettings<typename PagingTraits::MemoryType>;

  constexpr MapSettings kSettings = {
      .access =
          {
              .readable = true,
              .writable = true,
              .executable = true,
              .user_accessible = true,
          },
  };

  {
    constexpr uint64_t kPagePaddr = 0xffff'ffff'1000;
    constexpr uint64_t kPageVaddr = VaddrHighBits | kPagePaddr;

    PagingHelper<PagingTraits> helper;
    Table& root = helper.NewTable();

    auto result1 =
        Paging::template Map(root.paddr(), helper.MakePaddrToIo(), helper.MakeAllocator(), state,
                             kPageVaddr, 0x1000, kPagePaddr, kSettings);
    ASSERT_TRUE(result1.is_ok());

    auto result2 =
        Paging::template Map(root.paddr(), helper.MakePaddrToIo(), helper.MakeAllocator(), state,
                             kPageVaddr, 0x1000, kPagePaddr, kSettings);
    ASSERT_TRUE(result2.is_error());
    auto error = std::move(result2).error_value();
    EXPECT_EQ(arch::MapError::Type::kAlreadyMapped, error.type);
    EXPECT_EQ(kPageVaddr, error.vaddr);
  }
  {
    constexpr uint64_t kPagePaddr = 0xffff'ff20'0000;
    constexpr uint64_t kPageVaddr = VaddrHighBits | kPagePaddr;

    PagingHelper<PagingTraits> helper;
    Table& root = helper.NewTable();

    auto result1 =
        Paging::template Map(root.paddr(), helper.MakePaddrToIo(), helper.MakeAllocator(), state,
                             kPageVaddr, 0x20'0000, kPagePaddr, kSettings);
    ASSERT_TRUE(result1.is_ok());

    auto result2 =
        Paging::template Map(root.paddr(), helper.MakePaddrToIo(), helper.MakeAllocator(), state,
                             kPageVaddr, 0x20'0000, kPagePaddr, kSettings);
    ASSERT_TRUE(result2.is_error());
    auto error = std::move(result2).error_value();
    EXPECT_EQ(arch::MapError::Type::kAlreadyMapped, error.type);
    EXPECT_EQ(kPageVaddr, error.vaddr);
  }
  {
    constexpr uint64_t kPagePaddr = 0xffff'4000'0000;
    constexpr uint64_t kPageVaddr = VaddrHighBits | kPagePaddr;

    PagingHelper<PagingTraits> helper;
    Table& root = helper.NewTable();

    auto result1 =
        Paging::template Map(root.paddr(), helper.MakePaddrToIo(), helper.MakeAllocator(), state,
                             kPageVaddr, 0x4000'0000, kPagePaddr, kSettings);
    ASSERT_TRUE(result1.is_ok());

    auto result2 =
        Paging::template Map(root.paddr(), helper.MakePaddrToIo(), helper.MakeAllocator(), state,
                             kPageVaddr, 0x4000'0000, kPagePaddr, kSettings);
    ASSERT_TRUE(result2.is_error());
    auto error = std::move(result2).error_value();
    EXPECT_EQ(arch::MapError::Type::kAlreadyMapped, error.type);
    EXPECT_EQ(kPageVaddr, error.vaddr);
  }
}
TEST_FOR_ALL_TRAITS(DoubleMapping)

template <class PagingTraits, uint64_t VaddrHighBits = 0>
void MapAllocationFailure(typename PagingTraits::SystemState state) {
  using Paging = arch::Paging<PagingTraits>;
  using MapSettings = arch::MapSettings<typename PagingTraits::MemoryType>;

  constexpr uint64_t kVaddr = 0xffff'ffff'1000;
  constexpr uint64_t kPaddr = 0x0000'0000'1000;

  constexpr MapSettings kSettings = {
      .access =
          {
              .readable = true,
              .writable = true,
              .executable = true,
              .user_accessible = true,
          },
  };

  // If we fail on the i'th allocation for i=0,1,2, then the whole map attempt
  // should fail (as we were unable to allocate any of the three levels of
  // tables past the root).
  for (size_t i = 0; i < 3; ++i) {
    PagingHelper<PagingTraits> helper;
    Table& root = helper.NewTable();

    auto allocator = [&helper, j = size_t{0}, i](
                         uint64_t size, uint64_t alignment) mutable -> std::optional<uint64_t> {
      if (j++ < i) {
        return helper.MakeAllocator()(size, alignment);
      }
      return {};
    };
    auto result = Paging::template Map(root.paddr(), helper.MakePaddrToIo(), allocator, state,
                                       kVaddr, 0x1000, kPaddr, kSettings);
    ASSERT_TRUE(result.is_error());
    auto error = std::move(result).error_value();
    EXPECT_EQ(arch::MapError::Type::kAllocationFailure, error.type);
    EXPECT_EQ(error.vaddr, kVaddr);
  }
}
TEST_FOR_ALL_TRAITS(MapAllocationFailure)

template <class PagingTraits, uint64_t VaddrHighBits = 0>
void MappedRegionWithMultipleMappings(typename PagingTraits::SystemState state) {
  using Paging = arch::Paging<PagingTraits>;
  using MapSettings = arch::MapSettings<typename PagingTraits::MemoryType>;

  constexpr uint64_t k1GiB = 0x4000'0000;
  constexpr uint64_t k2MiB = 0x0020'0000;
  constexpr uint64_t k4KiB = 0x0000'1000;

  // Consider an mapped region of size 2GiB + 6MiB + 36KiB that begins at an
  // address (2MiB + 4KiB) shy of a 1GiB alignment. We'd expect the following
  // mappings in order:
  // * 1 4KiB page
  // * 1 2MiB page
  // * If 1GiB pages are supported, two 1GiB pages; else 512 2MiB ones
  // * 2 2MiB pages
  // * 8 4KiB pages.
  constexpr uint64_t kPaddr = 0xffff'0000'0000 - k2MiB - k4KiB;
  constexpr uint64_t kVaddr = VaddrHighBits | kPaddr;
  constexpr uint64_t kSize = 2 * k1GiB + 3 * k2MiB + 9 * k4KiB;

  constexpr MapSettings kSettings = {
      .access =
          {
              .readable = true,
              .writable = true,
              .executable = true,
              .user_accessible = true,
          },
  };

  PagingHelper<PagingTraits> helper;
  Table& root = helper.NewTable();

  {
    auto result = Paging::template Map(root.paddr(), helper.MakePaddrToIo(), helper.MakeAllocator(),
                                       state, kVaddr, kSize, kPaddr, kSettings);
    ASSERT_TRUE(result.is_ok());
  }

  {
    auto result = Paging::template Query(root.paddr(), helper.MakePaddrToIo(), kVaddr);
    ASSERT_TRUE(result.is_ok());

    auto& page = result->page;
    EXPECT_EQ(0x1000u, page.size);
    EXPECT_EQ(kPaddr, page.paddr);
  }
  {
    constexpr uint64_t kOffset = 0x1000u;
    auto result = Paging::template Query(root.paddr(), helper.MakePaddrToIo(), kVaddr + kOffset);
    ASSERT_TRUE(result.is_ok());

    auto& page = result->page;
    EXPECT_EQ(0x20'0000u, page.size);
    EXPECT_EQ(kPaddr + kOffset, page.paddr);
  }
  if (OneGibPagesAllowed(state)) {
    for (size_t j = 0; j < 2; ++j) {
      uint64_t offset = 0x1000u + 0x20'0000u + j * 0x4000'0000u;
      auto result = Paging::template Query(root.paddr(), helper.MakePaddrToIo(), kVaddr + offset);
      ASSERT_TRUE(result.is_ok());

      auto& page = result->page;
      EXPECT_EQ(0x4000'0000u, page.size);
      EXPECT_EQ(kPaddr + offset, page.paddr);
    }
  } else {
    for (size_t j = 0; j < 1024; ++j) {
      uint64_t offset = 0x1000u + 0x20'0000u + j * 0x20'0000u;

      auto result = Paging::template Query(root.paddr(), helper.MakePaddrToIo(), kVaddr + offset);
      ASSERT_TRUE(result.is_ok());

      auto& page = result->page;
      EXPECT_EQ(0x20'0000u, page.size);
      EXPECT_EQ(kPaddr + offset, page.paddr);
    }
  }
  for (size_t j = 0; j < 2; ++j) {
    uint64_t offset = 0x1000u + 0x20'0000u + 2 * 0x4000'0000u + j * 0x20'0000u;

    auto result = Paging::template Query(root.paddr(), helper.MakePaddrToIo(), kVaddr + offset);
    ASSERT_TRUE(result.is_ok());

    auto& page = result->page;
    EXPECT_EQ(0x20'0000u, page.size);
    EXPECT_EQ(kPaddr + offset, page.paddr);
  }
  for (size_t j = 0; j < 8; ++j) {
    uint64_t offset = 0x1000u + 0x20'0000u + 2 * 0x4000'0000u + 2 * 0x20'0000u + j * 0x1000u;

    auto result = Paging::template Query(root.paddr(), helper.MakePaddrToIo(), kVaddr + offset);
    ASSERT_TRUE(result.is_ok());

    auto& page = result->page;
    EXPECT_EQ(0x1000u, page.size);
    EXPECT_EQ(kPaddr + offset, page.paddr);
  }
}
TEST_FOR_ALL_TRAITS(MappedRegionWithMultipleMappings)

}  // namespace
