// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <lib/ddk/platform-defs.h>

#include <zxtest/zxtest.h>

#include "usage_pixel_format_cost.h"

namespace sysmem_driver {
namespace {

TEST(PixelFormatCost, Afbc) {
  fuchsia_sysmem2::BufferCollectionConstraints constraints;
  constraints.image_format_constraints().emplace(2);
  {
    fuchsia_sysmem2::ImageFormatConstraints image_format_constraints;
    {
      fuchsia_images2::PixelFormat pixel_format;
      pixel_format = fuchsia_images2::PixelFormat::kBgra32;
      image_format_constraints.pixel_format().emplace(std::move(pixel_format));
    }
    constraints.image_format_constraints()->at(0) = std::move(image_format_constraints);
  }
  {
    fuchsia_sysmem2::ImageFormatConstraints image_format_constraints;
    {
      fuchsia_images2::PixelFormat pixel_format;
      pixel_format = fuchsia_images2::PixelFormat::kBgra32;
      image_format_constraints.pixel_format() = pixel_format;
      image_format_constraints.pixel_format_modifier() =
          fuchsia_images2::kFormatModifierArmAfbc32X8;
    }
    constraints.image_format_constraints()->at(1) = std::move(image_format_constraints);
  }

  uint32_t amlogic_pids[] = {
      PDEV_PID_AMLOGIC_S912,
      PDEV_PID_AMLOGIC_S905D2,
      PDEV_PID_AMLOGIC_T931,
      PDEV_PID_AMLOGIC_A311D,
  };
  for (uint32_t pid : amlogic_pids) {
    EXPECT_LT(0, UsagePixelFormatCost::Compare(PDEV_VID_AMLOGIC, pid, constraints, 0, 1));
    EXPECT_GT(0, UsagePixelFormatCost::Compare(PDEV_VID_AMLOGIC, pid, constraints, 1, 0));
  }
  EXPECT_EQ(0, UsagePixelFormatCost::Compare(0u, PDEV_PID_AMLOGIC_S912, constraints, 0, 1));
  EXPECT_EQ(0, UsagePixelFormatCost::Compare(0u, PDEV_PID_AMLOGIC_S912, constraints, 1, 0));
}

TEST(PixelFormatCost, IntelTiling) {
  constexpr uint32_t kUnknownPid = 0;
  constexpr uint32_t kUnknownVid = 0;

  fuchsia_sysmem2::BufferCollectionConstraints constraints;

  constraints.image_format_constraints().emplace(2);
  uint64_t tiling_types[] = {fuchsia_images2::kFormatModifierIntelI915XTiled,
                             fuchsia_images2::kFormatModifierIntelI915YfTiled,
                             fuchsia_images2::kFormatModifierIntelI915YTiled};
  for (auto modifier : tiling_types) {
    {
      fuchsia_sysmem2::ImageFormatConstraints image_format_constraints;
      {
        fuchsia_images2::PixelFormat pixel_format;
        pixel_format = fuchsia_images2::PixelFormat::kBgra32;
        image_format_constraints.pixel_format() = pixel_format;
        image_format_constraints.pixel_format_modifier() = fuchsia_images2::kFormatModifierLinear;
      }
      constraints.image_format_constraints()->at(0) = std::move(image_format_constraints);
    }
    {
      fuchsia_sysmem2::ImageFormatConstraints image_format_constraints;
      {
        fuchsia_images2::PixelFormat pixel_format;
        pixel_format = fuchsia_images2::PixelFormat::kBgra32;
        image_format_constraints.pixel_format() = pixel_format;
        image_format_constraints.pixel_format_modifier() = modifier;
      }
      constraints.image_format_constraints()->at(1) = std::move(image_format_constraints);
    }
    EXPECT_LT(0, UsagePixelFormatCost::Compare(kUnknownVid, kUnknownPid, constraints, 0, 1));
    EXPECT_GT(0, UsagePixelFormatCost::Compare(kUnknownVid, kUnknownPid, constraints, 1, 0));
    // Intel tiled formats aren't necessarily useful on AMLOGIC, but if some hardware supported them
    // they should probably be used anyway.
    EXPECT_LT(0, UsagePixelFormatCost::Compare(PDEV_VID_AMLOGIC, PDEV_PID_AMLOGIC_S912, constraints,
                                               0, 1));
    EXPECT_GT(0, UsagePixelFormatCost::Compare(PDEV_VID_AMLOGIC, PDEV_PID_AMLOGIC_S912, constraints,
                                               1, 0));

    // Explicit linear should be treated the same as no format modifier value.
    constraints.image_format_constraints()->at(0).pixel_format_modifier() =
        fuchsia_images2::kFormatModifierNone;

    EXPECT_LT(0, UsagePixelFormatCost::Compare(kUnknownVid, kUnknownPid, constraints, 0, 1));
    EXPECT_GT(0, UsagePixelFormatCost::Compare(kUnknownVid, kUnknownPid, constraints, 1, 0));

    // Explicit linear should be treated the same as no format modifier value.
    {
      fuchsia_images2::PixelFormat pixel_format;
      pixel_format = fuchsia_images2::PixelFormat::kBgra32;
      constraints.image_format_constraints()->at(0).pixel_format() = pixel_format;
    }
    EXPECT_LT(0, UsagePixelFormatCost::Compare(kUnknownVid, kUnknownPid, constraints, 0, 1));
    EXPECT_GT(0, UsagePixelFormatCost::Compare(kUnknownVid, kUnknownPid, constraints, 1, 0));
  }

  // Formats are in ascending preference order (descending cost order).
  std::array modifier_list = {
      fuchsia_images2::kFormatModifierLinear,
      fuchsia_images2::kFormatModifierIntelI915XTiled,
      fuchsia_images2::kFormatModifierIntelI915YTiled,
      fuchsia_images2::kFormatModifierIntelI915YfTiled,
      fuchsia_images2::kFormatModifierIntelI915YTiledCcs,
      fuchsia_images2::kFormatModifierIntelI915YfTiledCcs,
  };
  constraints.image_format_constraints().emplace(modifier_list.size());

  for (uint32_t i = 0; i < modifier_list.size(); ++i) {
    {
      fuchsia_sysmem2::ImageFormatConstraints image_format_constraints;
      {
        fuchsia_images2::PixelFormat pixel_format;
        pixel_format = fuchsia_images2::PixelFormat::kBgra32;
        image_format_constraints.pixel_format() = pixel_format;
        image_format_constraints.pixel_format_modifier() = modifier_list[i];
      }
      constraints.image_format_constraints()->at(i) = std::move(image_format_constraints);
    }
  }

  for (uint32_t i = 1; i < modifier_list.size(); ++i) {
    EXPECT_LT(0, UsagePixelFormatCost::Compare(kUnknownVid, kUnknownPid, constraints, i - 1, i),
              "i=%d", i);
    EXPECT_GT(0, UsagePixelFormatCost::Compare(kUnknownVid, kUnknownPid, constraints, i, i - 1),
              "i=%d", i);
  }
}

TEST(PixelFormatCost, ArmTransactionElimination) {
  fuchsia_sysmem2::BufferCollectionConstraints constraints;
  constraints.image_format_constraints().emplace(2);
  {
    fuchsia_sysmem2::ImageFormatConstraints image_format_constraints;
    {
      fuchsia_images2::PixelFormat pixel_format;
      pixel_format = fuchsia_images2::PixelFormat::kBgra32;
      image_format_constraints.pixel_format() = pixel_format;
      image_format_constraints.pixel_format_modifier() =
          fuchsia_images2::kFormatModifierArmAfbc32X8;
    }
    constraints.image_format_constraints()->at(0) = std::move(image_format_constraints);
  }
  {
    fuchsia_sysmem2::ImageFormatConstraints image_format_constraints;
    {
      fuchsia_images2::PixelFormat pixel_format;
      pixel_format = fuchsia_images2::PixelFormat::kBgra32;
      image_format_constraints.pixel_format() = pixel_format;
      image_format_constraints.pixel_format_modifier() =
          fuchsia_images2::kFormatModifierArmAfbc32X8Te;
    }
    constraints.image_format_constraints()->at(1) = std::move(image_format_constraints);
  }

  EXPECT_LT(
      0, UsagePixelFormatCost::Compare(PDEV_VID_AMLOGIC, PDEV_PID_AMLOGIC_S912, constraints, 0, 1));
  EXPECT_GT(
      0, UsagePixelFormatCost::Compare(PDEV_VID_AMLOGIC, PDEV_PID_AMLOGIC_S912, constraints, 1, 0));
  EXPECT_EQ(0, UsagePixelFormatCost::Compare(0u, PDEV_PID_AMLOGIC_S912, constraints, 0, 1));
  EXPECT_EQ(0, UsagePixelFormatCost::Compare(0u, PDEV_PID_AMLOGIC_S912, constraints, 1, 0));
}

TEST(PixelFormatCost, AfbcWithFlags) {
  // Formats are in ascending preference order (descending cost order).
  std::array modifier_list = {
      fuchsia_images2::kFormatModifierLinear,
      fuchsia_images2::kFormatModifierArmAfbc16X16,
      fuchsia_images2::kFormatModifierArmAfbc16X16SplitBlockSparseYuv,
      fuchsia_images2::kFormatModifierArmAfbc16X16SplitBlockSparseYuvTiledHeader,
      fuchsia_images2::kFormatModifierArmAfbc16X16Te,
      fuchsia_images2::kFormatModifierArmAfbc16X16SplitBlockSparseYuvTe,
      fuchsia_images2::kFormatModifierArmAfbc16X16SplitBlockSparseYuvTeTiledHeader,
  };
  fuchsia_sysmem2::BufferCollectionConstraints constraints;
  constraints.image_format_constraints().emplace(modifier_list.size());

  for (uint32_t i = 0; i < modifier_list.size(); ++i) {
    {
      fuchsia_sysmem2::ImageFormatConstraints image_format_constraints;
      {
        fuchsia_images2::PixelFormat pixel_format;
        pixel_format = fuchsia_images2::PixelFormat::kBgra32;
        image_format_constraints.pixel_format() = pixel_format;
        image_format_constraints.pixel_format_modifier() = modifier_list[i];
      }
      constraints.image_format_constraints()->at(i) = std::move(image_format_constraints);
    }
  }

  for (uint32_t i = 1; i < modifier_list.size(); ++i) {
    EXPECT_LT(0,
              UsagePixelFormatCost::Compare(PDEV_VID_AMLOGIC, PDEV_PID_AMLOGIC_S912, constraints,
                                            i - 1, i),
              "i=%d", i);
    EXPECT_GT(0,
              UsagePixelFormatCost::Compare(PDEV_VID_AMLOGIC, PDEV_PID_AMLOGIC_S912, constraints, i,
                                            i - 1),
              "i=%d", i);
    EXPECT_EQ(0, UsagePixelFormatCost::Compare(0u, PDEV_PID_AMLOGIC_S912, constraints, i - 1, i));
    EXPECT_EQ(0, UsagePixelFormatCost::Compare(0u, PDEV_PID_AMLOGIC_S912, constraints, i, i - 1));
  }
}

}  // namespace
}  // namespace sysmem_driver
