// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/zbi_parser/bootfs.h>
#include <lib/mistos/zbi_parser/zbi.h>
#include <lib/mistos/zx/vmar.h>
#include <lib/mistos/zx/vmo.h>
#include <zircon/assert.h>

#include <fbl/alloc_checker.h>
#include <fbl/string.h>
#include <ktl/span.h>
#include <zxtest/zxtest.h>

#include "data/bootfs.zbi.h"

namespace {

const char* kA_TxTContent =
    "Four score and seven years ago our fathers brought forth on this "
    "continent, a new nation, conceived in Liberty, and dedicated to the "
    "proposition that all men are created equal.";

void CreateVmoFromZbiData(ktl::span<const char> buff, zx::vmo* out) {
  zx::vmo vmo;
  ASSERT_EQ(zx::vmo::create(buff.size(), 0, &vmo), ZX_OK);
  ASSERT_EQ(vmo.write(buff.data(), 0u, buff.size()), ZX_OK);
  *out = ktl::move(vmo);
}

class BootFs : public zxtest::Test {
 public:
  static void SetUpTestSuite() {
    ktl::span<const char> zbi{kBootFsZbi, sizeof(kBootFsZbi) - 1};
    CreateVmoFromZbiData(zbi, &boof_fs_zbi);
  }

  static zx::vmo boof_fs_zbi;
  static zx::resource boof_fs_vmex;
};

zx::vmo BootFs::boof_fs_zbi;
zx::resource BootFs::boof_fs_vmex;

TEST_F(BootFs, Ctor) {
  const zx::unowned_vmo zbi{BootFs::boof_fs_zbi};
  zx::unowned_vmar vmar_self = zx::vmar::root_self();

  auto result = zbi_parser::GetBootfsFromZbi(*vmar_self, *zbi, false);
  ASSERT_FALSE(result.is_error());
  zbi_parser::Bootfs bootfs{vmar_self->borrow(), ktl::move(result.value()),
                            ktl::move(BootFs::boof_fs_vmex)};
  ASSERT_TRUE(bootfs.is_valid());
}

TEST_F(BootFs, Open) {
  const zx::unowned_vmo zbi{BootFs::boof_fs_zbi};
  zx::unowned_vmar vmar_self = zx::vmar::root_self();

  auto result = zbi_parser::GetBootfsFromZbi(*vmar_self, *zbi, false);
  ASSERT_FALSE(result.is_error());

  zbi_parser::Bootfs bootfs{vmar_self->borrow(), std::move(result.value()),
                            ktl::move(BootFs::boof_fs_vmex)};
  ASSERT_TRUE(bootfs.is_valid());

  auto open_result = bootfs.Open("", "A.txt", "bootfs_test");
  ASSERT_TRUE(open_result.is_ok(), "error %d", open_result.error_value());

  auto file_vmo = ktl::move(open_result.value());
  ASSERT_TRUE(file_vmo.is_valid());

  uint64_t content_size;
  ASSERT_EQ(file_vmo.get_property(ZX_PROP_VMO_CONTENT_SIZE, &content_size, sizeof(content_size)),
            ZX_OK);

  ASSERT_EQ(strlen(kA_TxTContent), content_size);

  fbl::AllocChecker ac;
  char* payload = new (ac) char[content_size];
  ZX_ASSERT(ac.check());
  auto defer = fit::defer([&payload] { delete[] payload; });

  ASSERT_EQ(file_vmo.read(payload, 0, content_size), ZX_OK);

  EXPECT_BYTES_EQ(kA_TxTContent, payload, content_size);
}

}  // namespace
