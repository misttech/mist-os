// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_FLATLAND_FLATLAND_TYPES_H_
#define SRC_UI_SCENIC_LIB_FLATLAND_FLATLAND_TYPES_H_

#include <fuchsia/math/cpp/fidl.h>
#include <fuchsia/ui/composition/cpp/fidl.h>

#include <array>
#include <optional>

#include "src/ui/scenic/lib/types/rectangle.h"

#include <glm/glm.hpp>

namespace flatland {

// The sample region to use for an image when texturing a rectangle.
using ImageSampleRegion = fuchsia::math::RectF;

// The clip region for a transform to bound its children.
using TransformClipRegion = types::Rectangle;

// Represents an image rectangle, parameterized by an origin point, an extent representing the width
// and height. The texel UV coordinates specify, in clockwise order, the unnormalized clockwise
// texel coordinates beginning at the top-left coordinate (in texture-space). The orientation
// specifies the rotation applied to the rect. Note that origin and extent are specified in the
// new global coordinate-space (i.e. after all transforms have been applied).
struct ImageRect {
  ImageRect(const glm::vec2& origin, const glm::vec2& extent, const std::array<glm::ivec2, 4> uvs,
            fuchsia::ui::composition::Orientation orientation)
      : origin(origin), extent(extent), texel_uvs(uvs), orientation(orientation) {}

  // Creates an ImageRect with the specified width and height. |texel_uvs| are initialized using the
  // specified |extent| of the rectangle. Note that this may not be equal to the image you are
  // sampling from.
  ImageRect(const glm::vec2& origin, const glm::vec2& extent)
      : origin(origin),
        extent(extent),
        orientation(fuchsia::ui::composition::Orientation::CCW_0_DEGREES) {
    texel_uvs = {glm::vec2(0, 0), glm::vec2(extent.x, 0), glm::vec2(extent.x, extent.y),
                 glm::vec2(0, extent.y)};
  }

  ImageRect() = default;

  glm::vec2 origin = glm::vec2(0, 0);
  glm::vec2 extent = glm::vec2(1, 1);
  std::array<glm::ivec2, 4> texel_uvs = {glm::ivec2(0, 0), glm::ivec2(1, 0), glm::ivec2(1, 1),
                                         glm::ivec2(0, 1)};
  fuchsia::ui::composition::Orientation orientation;

  bool operator==(const ImageRect& other) const;
};

std::ostream& operator<<(std::ostream& str, const flatland::ImageRect& r);

// A flexible representation of a flatland hit region.
class HitRegion {
 public:
  // Finite hit region with default interaction.
  explicit HitRegion(fuchsia::math::RectF region,
                     fuchsia::ui::composition::HitTestInteraction interaction =
                         fuchsia::ui::composition::HitTestInteraction::DEFAULT);

  // Infinite hit region with default interaction.
  static HitRegion Infinite(fuchsia::ui::composition::HitTestInteraction interaction =
                                fuchsia::ui::composition::HitTestInteraction::DEFAULT);

  // Return true if region has finite extent.
  bool is_finite() const;

  // Finite region accessor. Caller ensures is_finite() is true.
  const fuchsia::math::RectF& region() const;

  // Hit test interaction accessor.
  fuchsia::ui::composition::HitTestInteraction interaction() const;

 private:
  // Helper for Infinite().
  explicit HitRegion(fuchsia::ui::composition::HitTestInteraction interaction);

  // Presence indicates a finite hit region.
  // Absence indicates an infinite hit region.
  std::optional<fuchsia::math::RectF> region_;

  fuchsia::ui::composition::HitTestInteraction interaction_ =
      fuchsia::ui::composition::HitTestInteraction::DEFAULT;
};

}  // namespace flatland

#endif  // SRC_UI_SCENIC_LIB_FLATLAND_FLATLAND_TYPES_H_
