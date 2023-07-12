// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/paging.h>
#include <lib/arch/riscv64/page-table.h>

#include <array>
#include <cstddef>
#include <cstdint>

#include <gtest/gtest.h>

namespace {

using AccessPermissions = arch::AccessPermissions;
using PagingSettings = arch::PagingSettings;
using RiscvPagingLevel = arch::RiscvPagingLevel;

constexpr bool kBools[] = {false, true};

constexpr AccessPermissions kMaximallyPermissive = {
    .readable = true,
    .writable = true,
    .executable = true,
    .user_accessible = true,
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

  entry.set_reg_value(0).Set(PagingSettings{
      .present = false,
  });
  EXPECT_FALSE(entry.v());
  EXPECT_FALSE(entry.present());

  entry.set_reg_value(0).Set(PagingSettings{
      .present = false,
  });
  EXPECT_FALSE(entry.v());
  EXPECT_FALSE(entry.present());

  entry.set_reg_value(0).Set(PagingSettings{
      .present = true,
      .terminal = false,
      .access = kMaximallyPermissive,
  });
  EXPECT_TRUE(entry.v());
  EXPECT_TRUE(entry.present());

  entry.set_reg_value(0).Set(PagingSettings{
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
          EXPECT_EQ(r || !w, arch::RiscvIsValidPageAccess(access));
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

  entry.set_reg_value(0).Set(PagingSettings{
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

  entry.set_reg_value(0).Set(PagingSettings{
      .present = true,
      .terminal = true,
      .access = AccessPermissions{.readable = true},
  });
  EXPECT_TRUE(entry.r());
  EXPECT_TRUE(entry.readable());

  entry.set_reg_value(0).Set(PagingSettings{
      .present = true,
      .terminal = false,
      .access = kMaximallyPermissive,
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

  entry.set_reg_value(0).Set(PagingSettings{
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

  entry.set_reg_value(0).Set(PagingSettings{
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

  entry.set_reg_value(0).Set(PagingSettings{
      .present = true,
      .terminal = false,
      .access = kMaximallyPermissive,
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

  entry.set_reg_value(0).Set(PagingSettings{
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

  entry.set_reg_value(0).Set(PagingSettings{
      .present = true,
      .terminal = true,
      .access = AccessPermissions{.executable = true},
  });
  EXPECT_TRUE(entry.x());
  EXPECT_TRUE(entry.executable());

  entry.set_reg_value(0).Set(PagingSettings{
      .present = true,
      .terminal = false,
      .access = kMaximallyPermissive,
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

  entry.set_reg_value(0).Set(PagingSettings{
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

  entry.set_reg_value(0).Set(PagingSettings{
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

  entry.set_reg_value(0).Set(PagingSettings{
      .present = true,
      .terminal = false,
      .access = kMaximallyPermissive,
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
        entry.set_reg_value(0).set_r(r).set_w(w).set_x(x).Set(PagingSettings{
            .present = true,
            .terminal = false,
            .access = kMaximallyPermissive,
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

  entry.set_reg_value(0).Set(PagingSettings{
      .present = true,
      .terminal = true,
      .access =
          AccessPermissions{
              .readable = true,
              .executable = false,
          },
  });
  EXPECT_TRUE(entry.terminal());

  entry.set_reg_value(0).Set(PagingSettings{
      .present = true,
      .terminal = true,
      .access =
          AccessPermissions{
              .readable = false,
              .executable = true,
          },
  });
  EXPECT_TRUE(entry.terminal());

  entry.set_reg_value(0).Set(PagingSettings{
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
    entry.set_reg_value(0).Set(PagingSettings{
        .address = addr,
        .present = true,
        .terminal = false,
        .access = kMaximallyPermissive,
    });
    EXPECT_EQ(addr >> 12, entry.ppn());
    EXPECT_EQ(addr, entry.address());
  }

  // The index for the first valid page address (per alignment constraints).
  constexpr size_t kFirstSupported = static_cast<size_t>(Level);

  for (size_t i = kFirstSupported; i < kAddrs.size(); ++i) {
    uint64_t addr = kAddrs[i];
    entry.set_reg_value(0).Set(PagingSettings{
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

}  // namespace
