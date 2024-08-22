// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-shim/devicetree-boot-shim.h>
#include <lib/boot-shim/devicetree.h>
#include <lib/boot-shim/testing/devicetree-test-fixture.h>
#include <lib/devicetree/testing/loaded-dtb.h>
#include <lib/fit/defer.h>
#include <lib/zbi-format/memory.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zbitl/image.h>

namespace {

using boot_shim::testing::LoadDtb;
using boot_shim::testing::LoadedDtb;

class RamoopsMatcherTest
    : public boot_shim::testing::TestMixin<boot_shim::testing::SyntheticDevicetreeTest> {
 public:
  static void SetUpTestSuite() {
    Mixin::SetUpTestSuite();
    auto loaded_dtb = LoadDtb("ramoops.dtb");
    ASSERT_TRUE(loaded_dtb.is_ok(), "%s", loaded_dtb.error_value().c_str());
    ramoops_ = std::move(loaded_dtb).value();
  }

  static void TearDownTestSuite() {
    ramoops_ = std::nullopt;
    Mixin::TearDownTestSuite();
  }

  devicetree::Devicetree ramoops() { return ramoops_->fdt(); }

 private:
  static std::optional<LoadedDtb> ramoops_;
};

std::optional<LoadedDtb> RamoopsMatcherTest::ramoops_ = std::nullopt;

TEST_F(RamoopsMatcherTest, GeneratesNoItemWhenMissing) {
  boot_shim::RamoopsMatcher ramoops_matcher("test", stdout);
  auto fdt = empty_fdt();
  ASSERT_TRUE(devicetree::Match(fdt, ramoops_matcher));
  ASSERT_FALSE(ramoops_matcher.range());
}

TEST_F(RamoopsMatcherTest, GeneratesItemWhenPresent) {
  boot_shim::RamoopsMatcher ramoops_matcher("test", stdout);
  auto fdt = ramoops();
  ASSERT_TRUE(devicetree::Match(fdt, ramoops_matcher));
  ASSERT_TRUE(ramoops_matcher.range());

  EXPECT_EQ(ramoops_matcher.range()->base, 0x8f000000);
  EXPECT_EQ(ramoops_matcher.range()->length, 0x100000);
}

}  // namespace
