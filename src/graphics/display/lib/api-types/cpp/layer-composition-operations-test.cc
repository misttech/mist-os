// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/layer-composition-operations.h"

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <gtest/gtest.h>

namespace display {

namespace {

constexpr LayerCompositionOperations kTransform2(
    fuchsia_hardware_display_engine::wire::LayerCompositionOperations::kTransform);

TEST(LayerCompositionOperationsTest, EqualityIsReflexive) {
  EXPECT_EQ(LayerCompositionOperations::kTransform, LayerCompositionOperations::kTransform);
  EXPECT_EQ(kTransform2, kTransform2);
  EXPECT_EQ(LayerCompositionOperations::kColorConversion,
            LayerCompositionOperations::kColorConversion);
}

TEST(LayerCompositionOperationsTest, EqualityIsSymmetric) {
  EXPECT_EQ(LayerCompositionOperations::kTransform, kTransform2);
  EXPECT_EQ(kTransform2, LayerCompositionOperations::kTransform);
}

TEST(LayerCompositionOperationsTest, EqualityForDifferentValues) {
  EXPECT_NE(LayerCompositionOperations::kTransform, LayerCompositionOperations::kColorConversion);
  EXPECT_NE(LayerCompositionOperations::kColorConversion, LayerCompositionOperations::kTransform);
  EXPECT_NE(kTransform2, LayerCompositionOperations::kColorConversion);
  EXPECT_NE(LayerCompositionOperations::kColorConversion, kTransform2);
}

TEST(LayerCompositionOperationsTest, ToBanjoLayerCompositionOperations) {
  static constexpr coordinate_transformation_t banjo_transformation =
      LayerCompositionOperations::kTransform.ToBanjo();
  EXPECT_EQ(LAYER_COMPOSITION_OPERATIONS_TRANSFORM, banjo_transformation);
}

TEST(LayerCompositionOperationsTest, ToFidlLayerCompositionOperations) {
  static constexpr fuchsia_hardware_display_engine::wire::LayerCompositionOperations
      fidl_transformation = LayerCompositionOperations::kTransform.ToFidl();
  EXPECT_EQ(fuchsia_hardware_display_engine::wire::LayerCompositionOperations::kTransform,
            fidl_transformation);
}

TEST(LayerCompositionOperationsTest, ValueForLogging) {
  EXPECT_EQ(0u, LayerCompositionOperations::kNoOperations.ValueForLogging());

  EXPECT_EQ(static_cast<uint32_t>(
                fuchsia_hardware_display_engine::wire::LayerCompositionOperations::kTransform),
            LayerCompositionOperations::kTransform.ValueForLogging());
}

TEST(LayerCompositionOperationsTest, DefaultConstructor) {
  EXPECT_EQ(LayerCompositionOperations::kNoOperations, LayerCompositionOperations{});
}

TEST(LayerCompositionOperationsTest, ToLayerCompositionOperationsWithBanjoValue) {
  static constexpr LayerCompositionOperations transformation(
      LAYER_COMPOSITION_OPERATIONS_TRANSFORM);
  EXPECT_EQ(LayerCompositionOperations::kTransform, transformation);
}

TEST(LayerCompositionOperationsTest, ToLayerCompositionOperationsWithFidlValue) {
  static constexpr LayerCompositionOperations transformation(
      fuchsia_hardware_display_engine::wire::LayerCompositionOperations::kTransform);
  EXPECT_EQ(LayerCompositionOperations::kTransform, transformation);
}

TEST(LayerCompositionOperationsTest, BanjoConversionRoundtrip) {
  EXPECT_EQ(LayerCompositionOperations::kTransform,
            LayerCompositionOperations(LayerCompositionOperations::kTransform.ToBanjo()));
  EXPECT_EQ(LayerCompositionOperations::kColorConversion,
            LayerCompositionOperations(LayerCompositionOperations::kColorConversion.ToBanjo()));
}

TEST(LayerCompositionOperationsTest, FidlConversionRoundtrip) {
  EXPECT_EQ(LayerCompositionOperations::kTransform,
            LayerCompositionOperations(LayerCompositionOperations::kTransform.ToFidl()));
  EXPECT_EQ(LayerCompositionOperations::kColorConversion,
            LayerCompositionOperations(LayerCompositionOperations::kColorConversion.ToFidl()));
}

TEST(LayerCompositionOperationsTest, IsValidFidlNoOperations) {
  static constexpr auto kNoOperation =
      static_cast<fuchsia_hardware_display_engine::wire::LayerCompositionOperations>(0);
  EXPECT_TRUE(LayerCompositionOperations::IsValid(kNoOperation));
}

TEST(LayerCompositionOperationsTest, IsValidBanjoNoOperations) {
  static constexpr auto kNoOperation = static_cast<layer_composition_operations_t>(0);
  EXPECT_TRUE(LayerCompositionOperations::IsValid(kNoOperation));
}

TEST(LayerCompositionOperationsTest, IsValidFidlUnknownOperations) {
  static constexpr auto kInvalidAllBits =
      static_cast<fuchsia_hardware_display_engine::wire::LayerCompositionOperations>(
          std::numeric_limits<uint32_t>::max());
  EXPECT_FALSE(LayerCompositionOperations::IsValid(kInvalidAllBits));
}

TEST(LayerCompositionOperationsTest, IsValidBanjoUnknownOperations) {
  static constexpr auto kInvalidAllBits =
      static_cast<layer_composition_operations_t>(std::numeric_limits<uint32_t>::max());
  EXPECT_FALSE(LayerCompositionOperations::IsValid(kInvalidAllBits));
}

TEST(LayerCompositionOperationsTest, HasUseImage) {
  EXPECT_EQ(false, LayerCompositionOperations::kNoOperations.HasUseImage());
  EXPECT_EQ(false, LayerCompositionOperations::kMergeBase.HasUseImage());

  EXPECT_EQ(true, LayerCompositionOperations::kUseImage.HasUseImage());
  EXPECT_EQ(true, LayerCompositionOperations::kAllOperations.HasUseImage());
}

TEST(LayerCompositionOperationsTest, WithUseImage) {
  EXPECT_EQ(LayerCompositionOperations::kUseImage,
            LayerCompositionOperations::kNoOperations.WithUseImage());
  EXPECT_EQ(LayerCompositionOperations::kUseImage,
            LayerCompositionOperations::kUseImage.WithUseImage());
  EXPECT_EQ(LayerCompositionOperations::kAllOperations,
            LayerCompositionOperations::kAllOperations.WithUseImage());
}

TEST(LayerCompositionOperationsTest, HasMergeBase) {
  EXPECT_EQ(false, LayerCompositionOperations::kNoOperations.HasMergeBase());
  EXPECT_EQ(false, LayerCompositionOperations::kMergeSrc.HasMergeBase());

  EXPECT_EQ(true, LayerCompositionOperations::kMergeBase.HasMergeBase());
  EXPECT_EQ(true, LayerCompositionOperations::kAllOperations.HasMergeBase());
}

TEST(LayerCompositionOperationsTest, WithMergeBase) {
  EXPECT_EQ(LayerCompositionOperations::kMergeBase,
            LayerCompositionOperations::kNoOperations.WithMergeBase());
  EXPECT_EQ(LayerCompositionOperations::kMergeBase,
            LayerCompositionOperations::kMergeBase.WithMergeBase());
  EXPECT_EQ(LayerCompositionOperations::kAllOperations,
            LayerCompositionOperations::kAllOperations.WithMergeBase());
}

TEST(LayerCompositionOperationsTest, HasMergeSrc) {
  EXPECT_EQ(false, LayerCompositionOperations::kNoOperations.HasMergeSrc());
  EXPECT_EQ(false, LayerCompositionOperations::kFrameScale.HasMergeSrc());

  EXPECT_EQ(true, LayerCompositionOperations::kMergeSrc.HasMergeSrc());
  EXPECT_EQ(true, LayerCompositionOperations::kAllOperations.HasMergeSrc());
}

TEST(LayerCompositionOperationsTest, WithMergeSrc) {
  EXPECT_EQ(LayerCompositionOperations::kMergeSrc,
            LayerCompositionOperations::kNoOperations.WithMergeSrc());
  EXPECT_EQ(LayerCompositionOperations::kMergeSrc,
            LayerCompositionOperations::kMergeSrc.WithMergeSrc());
  EXPECT_EQ(LayerCompositionOperations::kAllOperations,
            LayerCompositionOperations::kAllOperations.WithMergeSrc());
}

TEST(LayerCompositionOperationsTest, HasFrameScale) {
  EXPECT_EQ(false, LayerCompositionOperations::kNoOperations.HasFrameScale());
  EXPECT_EQ(false, LayerCompositionOperations::kSrcFrame.HasFrameScale());

  EXPECT_EQ(true, LayerCompositionOperations::kFrameScale.HasFrameScale());
  EXPECT_EQ(true, LayerCompositionOperations::kAllOperations.HasFrameScale());
}

TEST(LayerCompositionOperationsTest, WithFrameScale) {
  EXPECT_EQ(LayerCompositionOperations::kFrameScale,
            LayerCompositionOperations::kNoOperations.WithFrameScale());
  EXPECT_EQ(LayerCompositionOperations::kFrameScale,
            LayerCompositionOperations::kFrameScale.WithFrameScale());
  EXPECT_EQ(LayerCompositionOperations::kAllOperations,
            LayerCompositionOperations::kAllOperations.WithFrameScale());
}

TEST(LayerCompositionOperationsTest, HasSrcFrame) {
  EXPECT_EQ(false, LayerCompositionOperations::kNoOperations.HasSrcFrame());
  EXPECT_EQ(false, LayerCompositionOperations::kTransform.HasSrcFrame());

  EXPECT_EQ(true, LayerCompositionOperations::kSrcFrame.HasSrcFrame());
  EXPECT_EQ(true, LayerCompositionOperations::kAllOperations.HasSrcFrame());
}

TEST(LayerCompositionOperationsTest, WithSrcFrame) {
  EXPECT_EQ(LayerCompositionOperations::kSrcFrame,
            LayerCompositionOperations::kNoOperations.WithSrcFrame());
  EXPECT_EQ(LayerCompositionOperations::kSrcFrame,
            LayerCompositionOperations::kSrcFrame.WithSrcFrame());
  EXPECT_EQ(LayerCompositionOperations::kAllOperations,
            LayerCompositionOperations::kAllOperations.WithSrcFrame());
}

TEST(LayerCompositionOperationsTest, HasTransform) {
  EXPECT_EQ(false, LayerCompositionOperations::kNoOperations.HasTransform());
  EXPECT_EQ(false, LayerCompositionOperations::kColorConversion.HasTransform());

  EXPECT_EQ(true, LayerCompositionOperations::kTransform.HasTransform());
  EXPECT_EQ(true, LayerCompositionOperations::kAllOperations.HasTransform());
}

TEST(LayerCompositionOperationsTest, WithTransform) {
  EXPECT_EQ(LayerCompositionOperations::kTransform,
            LayerCompositionOperations::kNoOperations.WithTransform());
  EXPECT_EQ(LayerCompositionOperations::kTransform,
            LayerCompositionOperations::kTransform.WithTransform());
  EXPECT_EQ(LayerCompositionOperations::kAllOperations,
            LayerCompositionOperations::kAllOperations.WithTransform());
}

TEST(LayerCompositionOperationsTest, HasColorConversion) {
  EXPECT_EQ(false, LayerCompositionOperations::kNoOperations.HasColorConversion());
  EXPECT_EQ(false, LayerCompositionOperations::kAlpha.HasColorConversion());

  EXPECT_EQ(true, LayerCompositionOperations::kColorConversion.HasColorConversion());
  EXPECT_EQ(true, LayerCompositionOperations::kAllOperations.HasColorConversion());
}

TEST(LayerCompositionOperationsTest, WithColorConversion) {
  EXPECT_EQ(LayerCompositionOperations::kColorConversion,
            LayerCompositionOperations::kNoOperations.WithColorConversion());
  EXPECT_EQ(LayerCompositionOperations::kColorConversion,
            LayerCompositionOperations::kColorConversion.WithColorConversion());
  EXPECT_EQ(LayerCompositionOperations::kAllOperations,
            LayerCompositionOperations::kAllOperations.WithColorConversion());
}

TEST(LayerCompositionOperationsTest, HasAlpha) {
  EXPECT_EQ(false, LayerCompositionOperations::kNoOperations.HasAlpha());
  EXPECT_EQ(false, LayerCompositionOperations::kUseImage.HasAlpha());

  EXPECT_EQ(true, LayerCompositionOperations::kAlpha.HasAlpha());
  EXPECT_EQ(true, LayerCompositionOperations::kAllOperations.HasAlpha());
}

TEST(LayerCompositionOperationsTest, WithAlpha) {
  EXPECT_EQ(LayerCompositionOperations::kAlpha,
            LayerCompositionOperations::kNoOperations.WithAlpha());
  EXPECT_EQ(LayerCompositionOperations::kAlpha, LayerCompositionOperations::kAlpha.WithAlpha());
  EXPECT_EQ(LayerCompositionOperations::kAllOperations,
            LayerCompositionOperations::kAllOperations.WithAlpha());
}

}  // namespace

}  // namespace display
