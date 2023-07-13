// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/arm64/page-table.h>
#include <lib/arch/paging.h>
#include <lib/arch/riscv64/page-table.h>
#include <lib/arch/x86/page-table.h>

#include <array>
#include <cstddef>
#include <cstdint>

#include <gtest/gtest.h>

namespace {

using AccessPermissions = arch::AccessPermissions;
using ArmPagingLevel = arch::ArmAddressTranslationLevel;
using PagingSettings = arch::PagingSettings;
using RiscvPagingLevel = arch::RiscvPagingLevel;
using X86PagingLevel = arch::X86PagingLevel;
using X86SystemState = arch::X86SystemPagingState;

template <ArmPagingLevel Level>
using ArmTableEntry = typename arch::ArmPagingTraits::template TableEntry<Level>;

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

constexpr X86SystemState kX86SystemStateNxDisallowed = {
    .nxe = false,
};

constexpr X86SystemState kX86SystemStateNxAllowed = {
    .nxe = true,
};

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

  entry.set_reg_value(0).Set({}, PagingSettings{.present = false});
  EXPECT_FALSE(entry.v());
  EXPECT_FALSE(entry.present());

  entry.set_reg_value(0).Set({}, PagingSettings{.present = false});
  EXPECT_FALSE(entry.v());
  EXPECT_FALSE(entry.present());

  entry.set_reg_value(0).Set({}, PagingSettings{
                                     .present = true,
                                     .terminal = false,
                                     .access = kRWXU,
                                 });
  EXPECT_TRUE(entry.v());
  EXPECT_TRUE(entry.present());

  entry.set_reg_value(0).Set({}, PagingSettings{
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
      {}, PagingSettings{
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

  entry.set_reg_value(0).Set({}, PagingSettings{
                                     .present = true,
                                     .terminal = true,
                                     .access = AccessPermissions{.readable = true},
                                 });
  EXPECT_TRUE(entry.r());
  EXPECT_TRUE(entry.readable());

  entry.set_reg_value(0).Set({}, PagingSettings{
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
      {}, PagingSettings{
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
      {}, PagingSettings{
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

  entry.set_reg_value(0).Set({}, PagingSettings{
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
      {}, PagingSettings{
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

  entry.set_reg_value(0).Set({}, PagingSettings{
                                     .present = true,
                                     .terminal = true,
                                     .access = AccessPermissions{.executable = true},
                                 });
  EXPECT_TRUE(entry.x());
  EXPECT_TRUE(entry.executable());

  entry.set_reg_value(0).Set({}, PagingSettings{
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

  entry.set_reg_value(0).Set({}, PagingSettings{
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

  entry.set_reg_value(0).Set({}, PagingSettings{
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

  entry.set_reg_value(0).Set({}, PagingSettings{
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
        entry.set_reg_value(0).set_r(r).set_w(w).set_x(x).Set({}, PagingSettings{
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

  entry.set_reg_value(0).Set({}, PagingSettings{
                                     .present = true,
                                     .terminal = true,
                                     .access =
                                         AccessPermissions{
                                             .readable = true,
                                             .executable = false,
                                         },
                                 });
  EXPECT_TRUE(entry.terminal());

  entry.set_reg_value(0).Set({}, PagingSettings{
                                     .present = true,
                                     .terminal = true,
                                     .access =
                                         AccessPermissions{
                                             .readable = false,
                                             .executable = true,
                                         },
                                 });
  EXPECT_TRUE(entry.terminal());

  entry.set_reg_value(0).Set({}, PagingSettings{
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
    entry.set_reg_value(0).Set({}, PagingSettings{
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
    entry.set_reg_value(0).Set({}, PagingSettings{
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

template <X86PagingLevel Level>
void TestX86GetSetPresent() {
  // Presence is controlled by the P bit.
  arch::X86PagingStructure<Level> entry;

  entry.set_reg_value(0).set_p(false);
  EXPECT_FALSE(entry.set_reg_value(0).present());

  entry.set_reg_value(0).set_p(true);
  EXPECT_TRUE(entry.present());

  if constexpr (kX86LevelCanBeNonTerminal<Level>) {
    entry.set_reg_value(0).Set({},
                               PagingSettings{.present = false, .terminal = false, .access = kRX});
    EXPECT_FALSE(entry.p());
    EXPECT_FALSE(entry.present());

    entry.set_reg_value(0).Set({},
                               PagingSettings{.present = true, .terminal = false, .access = kRX});
    EXPECT_TRUE(entry.p());
    EXPECT_TRUE(entry.present());
  }

  if constexpr (kX86LevelCanBeTerminal<Level>) {
    entry.set_reg_value(0).Set({},
                               PagingSettings{.present = false, .terminal = true, .access = kRX});
    EXPECT_FALSE(entry.p());
    EXPECT_FALSE(entry.present());

    entry.set_reg_value(0).Set({},
                               PagingSettings{.present = true, .terminal = true, .access = kRX});
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
          EXPECT_EQ(r && x, arch::X86IsValidPageAccess(kX86SystemStateNxDisallowed, access));
          EXPECT_EQ(r, arch::X86IsValidPageAccess(kX86SystemStateNxAllowed, access));
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
    entry.set_reg_value(0).Set({}, PagingSettings{
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
        PagingSettings{
            .present = true,
            .terminal = false,
            .access = AccessPermissions{.readable = true, .writable = false, .executable = true},
        });
    EXPECT_FALSE(entry.r_w());
    EXPECT_FALSE(entry.writable());

    entry.set_reg_value(0).Set(
        {}, PagingSettings{
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
        PagingSettings{
            .present = true,
            .terminal = true,
            .access = AccessPermissions{.readable = true, .writable = false, .executable = true},
        });
    EXPECT_FALSE(entry.r_w());
    EXPECT_FALSE(entry.writable());

    entry.set_reg_value(0).Set(
        {}, PagingSettings{
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
        kX86SystemStateNxAllowed,
        PagingSettings{
            .present = true,
            .terminal = false,
            .access = AccessPermissions{.readable = true, .executable = false},
        });
    EXPECT_TRUE(entry.xd());
    EXPECT_FALSE(entry.executable());

    entry.set_reg_value(0).set_xd(false).Set(kX86SystemStateNxDisallowed, PagingSettings{
                                                                              .present = true,
                                                                              .terminal = false,
                                                                              .access = kRX,
                                                                          });
    EXPECT_FALSE(entry.xd());
    EXPECT_TRUE(entry.executable());

    entry.set_reg_value(0).set_xd(false).Set(kX86SystemStateNxAllowed, PagingSettings{
                                                                           .present = true,
                                                                           .terminal = false,
                                                                           .access = kRX,
                                                                       });
    EXPECT_FALSE(entry.xd());
  }

  if constexpr (kX86LevelCanBeTerminal<Level>) {
    entry.set_reg_value(0).set_xd(false).Set(
        kX86SystemStateNxAllowed,
        PagingSettings{
            .present = true,
            .terminal = true,
            .access = AccessPermissions{.readable = true, .executable = false},
        });
    EXPECT_TRUE(entry.xd());
    EXPECT_FALSE(entry.executable());

    entry.set_reg_value(0).set_xd(false).Set(kX86SystemStateNxDisallowed, PagingSettings{
                                                                              .present = true,
                                                                              .terminal = true,
                                                                              .access = kRX,
                                                                          });
    EXPECT_FALSE(entry.xd());
    EXPECT_TRUE(entry.executable());

    entry.set_reg_value(0).set_xd(false).Set(kX86SystemStateNxAllowed, PagingSettings{
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
        PagingSettings{
            .present = true,
            .terminal = false,
            .access =
                AccessPermissions{.readable = true, .executable = true, .user_accessible = false},
        });
    EXPECT_FALSE(entry.u_s());
    EXPECT_FALSE(entry.user_accessible());

    entry.set_reg_value(0).Set({}, PagingSettings{
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
        PagingSettings{
            .present = true,
            .terminal = true,
            .access =
                AccessPermissions{.readable = true, .executable = true, .user_accessible = false},
        });
    EXPECT_FALSE(entry.u_s());
    EXPECT_FALSE(entry.user_accessible());

    entry.set_reg_value(0).Set({}, PagingSettings{
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
    entry.set_reg_value(0).Set({},
                               PagingSettings{.present = true, .terminal = false, .access = kRX});
    EXPECT_FALSE(entry.terminal());
    if constexpr (kPdptOrkPd) {
      EXPECT_FALSE(*entry.ps());
    } else {
      EXPECT_EQ(std::nullopt, entry.ps());
    }
  }

  if constexpr (kX86LevelCanBeTerminal<Level>) {
    entry.set_reg_value(0).Set({},
                               PagingSettings{.present = true, .terminal = true, .access = kRX});
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
      entry.set_reg_value(0).Set({}, PagingSettings{
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
      entry.set_reg_value(0).Set({}, PagingSettings{
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

template <ArmPagingLevel Level>
void TestArmGetSetPresent() {
  ArmTableEntry<Level> entry;

  entry.set_reg_value(0).set_valid(false);
  EXPECT_FALSE(entry.present());

  entry.set_reg_value(0).set_valid(true);
  EXPECT_TRUE(entry.present());

  entry.set_reg_value(0).set_valid(true).Set({}, PagingSettings{.present = false});
  EXPECT_FALSE(entry.valid());
  EXPECT_FALSE(entry.present());

  if constexpr (kArmLevelCanBeNonTerminal<Level>) {
    entry.set_reg_value(0).set_valid(false).Set({}, PagingSettings{
                                                        .present = true,
                                                        .terminal = false,
                                                    });
    EXPECT_TRUE(entry.valid());
    EXPECT_TRUE(entry.present());
  }

  if constexpr (kArmLevelCanBeTerminal<Level>) {
    entry.set_reg_value(0).set_valid(false).Set({}, PagingSettings{
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

    entry.set_reg_value(0).Set({}, PagingSettings{.present = true, .terminal = true});
    EXPECT_TRUE(entry.terminal());
    EXPECT_TRUE(entry.IsPage());
  }

  if constexpr (Level == ArmPagingLevel::k1 || Level == ArmPagingLevel::k2) {
    entry.set_reg_value(0).SetAsBlock();
    EXPECT_TRUE(entry.terminal());

    entry.set_reg_value(0).Set({}, PagingSettings{.present = true, .terminal = true});
    EXPECT_TRUE(entry.terminal());
    EXPECT_TRUE(entry.IsBlock());
  }

  if constexpr (Level != ArmPagingLevel::k3) {
    entry.set_reg_value(0).SetAsTable();
    EXPECT_FALSE(entry.terminal());

    entry.set_reg_value(0).Set({}, PagingSettings{.present = true, .terminal = false});
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
    entry.set_reg_value(0).Set({}, PagingSettings{
                                       .present = true,
                                       .terminal = false,
                                       .access = AccessPermissions{.readable = true},
                                   });
    EXPECT_TRUE(entry.readable());
  }

  if constexpr (kArmLevelCanBeTerminal<Level>) {
    entry.set_reg_value(0).Set({}, PagingSettings{
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
    entry.set_reg_value(0).Set({}, PagingSettings{
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

    entry.set_reg_value(0).Set({}, PagingSettings{
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

    entry.set_reg_value(0).Set({}, PagingSettings{
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

    entry.set_reg_value(0).Set({}, PagingSettings{
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

    entry.set_reg_value(0).Set({}, PagingSettings{
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

    entry.set_reg_value(0).Set({}, PagingSettings{
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

    entry.set_reg_value(0).Set({}, PagingSettings{
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

    entry.set_reg_value(0).Set({}, PagingSettings{
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
      entry.set_reg_value(0).Set({}, PagingSettings{
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

      entry.set_reg_value(0).Set({}, PagingSettings{
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
      entry.set_reg_value(0).Set({}, PagingSettings{
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

      entry.set_reg_value(0).Set({}, PagingSettings{
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

      entry.set_reg_value(0).Set({}, PagingSettings{
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

      entry.set_reg_value(0).Set({}, PagingSettings{
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

}  // namespace
