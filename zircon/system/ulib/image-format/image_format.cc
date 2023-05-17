// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/image-format/image_format.h"

#include <fidl/fuchsia.images2/cpp/fidl.h>
#include <fidl/fuchsia.sysmem2/cpp/fidl.h>

#if defined(FIDL_ALLOW_DEPRECATED_C_BINDINGS)
#include <fuchsia/sysmem/c/fidl.h>
#endif
#include <lib/fidl/cpp/wire_natural_conversions.h>
#include <lib/sysmem-version/sysmem-version.h>
#include <lib/zbi-format/graphics.h>
#include <zircon/assert.h>

#include <algorithm>
#include <map>
#include <set>

#include <fbl/algorithm.h>
#include <safemath/safe_math.h>

using safemath::CheckAdd;
using safemath::CheckDiv;
using safemath::CheckMul;
using safemath::CheckSub;

namespace {

using ColorSpace = fuchsia_images2::ColorSpace;
using ImageFormat = fuchsia_images2::ImageFormat;
using ImageFormatConstraints = fuchsia_sysmem2::ImageFormatConstraints;
using PixelFormat = fuchsia_images2::PixelFormat;

using ColorSpaceWire = fuchsia_images2::wire::ColorSpace;
using ImageFormatWire = fuchsia_images2::wire::ImageFormat;
using ImageFormatConstraintsWire = fuchsia_sysmem2::wire::ImageFormatConstraints;
using PixelFormatWire = fuchsia_images2::wire::PixelFormat;

// There are two aspects of the ColorSpaceWire and PixelFormatWire that we care about:
//   * bits-per-sample - bits per primary sample (R, G, B, or Y)
//   * RGB vs. YUV - whether the system supports the ColorSpaceWire or PixelFormatWire
//     representing RGB data or YUV data.  Any given ColorSpaceWire only supports
//     one or the other. Currently any given PixelFormatWire only supports one or
//     the other and this isn't likely to change.
// While we could just list all the ColorSpaceWire(s) that each PixelFormatWire could
// plausibly support, expressing in terms of bits-per-sample and RGB vs. YUV is
// perhaps easier to grok.

enum ColorType { kColorType_NONE, kColorType_RGB, kColorType_YUV };

struct SamplingInfo {
  std::set<uint32_t> possible_bits_per_sample;
  ColorType color_type;
};

const std::map<ColorSpaceWire, SamplingInfo> kColorSpaceSamplingInfo = {
    {ColorSpace::kSrgb, {{8, 10, 12, 16}, kColorType_RGB}},
    {ColorSpace::kRec601Ntsc, {{8, 10}, kColorType_YUV}},
    {ColorSpace::kRec601NtscFullRange, {{8, 10}, kColorType_YUV}},
    {ColorSpace::kRec601Pal, {{8, 10}, kColorType_YUV}},
    {ColorSpace::kRec601PalFullRange, {{8, 10}, kColorType_YUV}},
    {ColorSpace::kRec709, {{8, 10}, kColorType_YUV}},
    {ColorSpace::kRec2020, {{10, 12}, kColorType_YUV}},
    {ColorSpace::kRec2100, {{10, 12}, kColorType_YUV}},
};
const std::map<PixelFormatWire, SamplingInfo> kPixelFormatSamplingInfo = {
    {PixelFormat::kR8G8B8A8, {{8}, kColorType_RGB}},
    {PixelFormat::kBgra32, {{8}, kColorType_RGB}},
    {PixelFormat::kI420, {{8}, kColorType_YUV}},
    {PixelFormat::kM420, {{8}, kColorType_YUV}},
    {PixelFormat::kNv12, {{8}, kColorType_YUV}},
    {PixelFormat::kYuy2, {{8}, kColorType_YUV}},
    // 8 bits RGB when uncompressed - in this context, MJPEG is essentially
    // pretending to be uncompressed.
    {PixelFormat::kMjpeg, {{8}, kColorType_RGB}},
    {PixelFormat::kYv12, {{8}, kColorType_YUV}},
    {PixelFormat::kBgr24, {{8}, kColorType_RGB}},

    // These use the same colorspaces as regular 8-bit-per-component formats
    {PixelFormat::kRgb565, {{8}, kColorType_RGB}},
    {PixelFormat::kRgb332, {{8}, kColorType_RGB}},
    {PixelFormat::kRgb2220, {{8}, kColorType_RGB}},
    // Expands to RGB
    {PixelFormat::kL8, {{8}, kColorType_RGB}},
    {PixelFormat::kR8, {{8}, kColorType_RGB}},
    {PixelFormat::kR8G8, {{8}, kColorType_RGB}},
    {PixelFormat::kA2B10G10R10, {{8}, kColorType_RGB}},
    {PixelFormat::kA2R10G10B10, {{8}, kColorType_RGB}},
};

constexpr uint32_t kTransactionEliminationAlignment = 64;
// The transaction elimination buffer is always reported as plane 3.
constexpr uint32_t kTransactionEliminationPlane = 3;

uint64_t arm_transaction_elimination_row_size(uint32_t width) {
  uint32_t kTileSize = 32;
  uint32_t kBytesPerTilePerRow = 16;
  uint32_t width_in_tiles = fbl::round_up(width, kTileSize) / kTileSize;
  return fbl::round_up(width_in_tiles * kBytesPerTilePerRow, kTransactionEliminationAlignment);
}

uint64_t arm_transaction_elimination_buffer_size(uint64_t start, uint32_t width, uint32_t height) {
  uint32_t kTileSize = 32;
  uint32_t end = start;
  end = fbl::round_up(end, kTransactionEliminationAlignment);
  uint32_t kHeaderSize = kTransactionEliminationAlignment;
  end += kHeaderSize;
  uint32_t height_in_tiles = fbl::round_up(height, kTileSize) / kTileSize;
  end += arm_transaction_elimination_row_size(width) * 2 * height_in_tiles;
  return end - start;
}

class ImageFormatSet {
 public:
  virtual const char* Name() const = 0;
  virtual bool IsSupported(const PixelFormatAndModifier& pixel_format) const = 0;
  virtual uint64_t ImageFormatImageSize(const ImageFormat& image_format) const = 0;
  virtual bool ImageFormatPlaneByteOffset(const ImageFormat& image_format, uint32_t plane,
                                          uint64_t* offset_out) const = 0;
  virtual bool ImageFormatPlaneRowBytes(const ImageFormat& image_format, uint32_t plane,
                                        uint32_t* row_bytes_out) const = 0;
  virtual bool ImageFormatMinimumRowBytes(
      const fuchsia_sysmem2::ImageFormatConstraints& constraints, uint32_t width,
      uint32_t* minimum_row_bytes_out) const = 0;
};

class IntelTiledFormats : public ImageFormatSet {
 public:
  const char* Name() const override { return "IntelTiledFormats"; }

  bool IsSupported(const PixelFormatAndModifier& pixel_format) const override {
    if (pixel_format.pixel_format != PixelFormat::kR8G8B8A8 &&
        pixel_format.pixel_format != PixelFormat::kBgra32 &&
        pixel_format.pixel_format != PixelFormat::kNv12) {
      return false;
    }
    switch (pixel_format.pixel_format_modifier) {
      case fuchsia_images2::kFormatModifierIntelI915XTiled:
      case fuchsia_images2::kFormatModifierIntelI915YTiled:
      case fuchsia_images2::kFormatModifierIntelI915YfTiled:
      // X-Tiled CCS is not supported.
      case fuchsia_images2::kFormatModifierIntelI915YTiledCcs:
      case fuchsia_images2::kFormatModifierIntelI915YfTiledCcs:
        return true;
      default:
        return false;
    }
  }

  uint64_t ImageFormatImageSize(const ImageFormat& image_format) const override {
    auto pixel_format_and_modifier = PixelFormatAndModifierFromImageFormat(image_format);
    ZX_DEBUG_ASSERT(IsSupported(PixelFormatAndModifier(pixel_format_and_modifier)));

    uint32_t width_in_tiles, height_in_tiles;
    uint32_t num_of_planes = FormatNumOfPlanes(image_format.pixel_format().value());
    uint64_t size = 0u;

    for (uint32_t plane_idx = 0; plane_idx < num_of_planes; plane_idx += 1) {
      GetSizeInTiles(image_format, plane_idx, &width_in_tiles, &height_in_tiles);
      size += (width_in_tiles * height_in_tiles * kIntelTileByteSize);
    }

    if (FormatHasCcs(pixel_format_and_modifier)) {
      size += CcsSize(width_in_tiles, height_in_tiles);
    }

    return size;
  }

  bool ImageFormatPlaneByteOffset(const ImageFormat& image_format, uint32_t plane,
                                  uint64_t* offset_out) const override {
    ZX_DEBUG_ASSERT(IsSupported(PixelFormatAndModifier(
        image_format.pixel_format().value(), image_format.pixel_format_modifier().value())));

    uint32_t num_of_planes = FormatNumOfPlanes(image_format.pixel_format().value());

    uint32_t end_plane;

    // For image data planes, calculate the size of all previous the image data planes
    if (plane < num_of_planes) {
      end_plane = plane;
    } else if (plane == kCcsPlane) {  // If requesting the CCS Aux plane, calculate the size of all
                                      // the image data planes
      end_plane = num_of_planes;
    } else {  // Plane is out of bounds, return false
      return false;
    }

    uint64_t offset = 0u;
    for (uint32_t plane_idx = 0u; plane_idx < end_plane; plane_idx += 1u) {
      uint32_t width_in_tiles, height_in_tiles;
      GetSizeInTiles(image_format, plane_idx, &width_in_tiles, &height_in_tiles);
      offset += (width_in_tiles * height_in_tiles * kIntelTileByteSize);
    }
    ZX_DEBUG_ASSERT(offset % kIntelTileByteSize == 0);
    *offset_out = offset;
    return true;
  }

  bool ImageFormatPlaneRowBytes(const ImageFormat& image_format, uint32_t plane,
                                uint32_t* row_bytes_out) const override {
    auto pixel_format_and_modifier = PixelFormatAndModifierFromImageFormat(image_format);
    ZX_DEBUG_ASSERT(IsSupported(pixel_format_and_modifier));

    uint32_t num_of_planes = FormatNumOfPlanes(image_format.pixel_format().value());

    if (plane < num_of_planes) {
      uint32_t width_in_tiles, height_in_tiles;
      GetSizeInTiles(image_format, plane, &width_in_tiles, &height_in_tiles);
      const auto& tiling_data = GetTilingData(GetTilingTypeForPixelFormat(PixelFormatAndModifier(
          image_format.pixel_format().value(), image_format.pixel_format_modifier().value())));
      *row_bytes_out = width_in_tiles * tiling_data.bytes_per_row_per_tile;
      return true;
    }

    if (plane == kCcsPlane && FormatHasCcs(pixel_format_and_modifier)) {
      uint32_t width_in_tiles, height_in_tiles;
      // Since we only care about the width, just use the first plane
      GetSizeInTiles(image_format, 0, &width_in_tiles, &height_in_tiles);
      *row_bytes_out =
          CcsWidthInTiles(width_in_tiles) * GetTilingData(TilingType::kY).bytes_per_row_per_tile;
      return true;
    }

    return false;
  }

  bool ImageFormatMinimumRowBytes(const fuchsia_sysmem2::ImageFormatConstraints& constraints,
                                  uint32_t width, uint32_t* minimum_row_bytes_out) const override {
    auto pixel_format_and_modifier = PixelFormatAndModifierFromConstraints(constraints);
    ZX_DEBUG_ASSERT(IsSupported(pixel_format_and_modifier));

    ZX_DEBUG_ASSERT(minimum_row_bytes_out);
    // Caller must set pixel_format.
    ZX_DEBUG_ASSERT(constraints.pixel_format().has_value());

    if (constraints.size_alignment().has_value()) {
      width = fbl::round_up(width, constraints.size_alignment()->width());
    }

    if ((constraints.min_size().has_value() && width < constraints.min_size()->width()) ||
        (constraints.max_size().has_value() && width > constraints.max_size()->width())) {
      return false;
    }
    uint32_t constraints_min_bytes_per_row =
        constraints.min_bytes_per_row().has_value() ? constraints.min_bytes_per_row().value() : 0;
    uint32_t constraints_bytes_per_row_divisor = constraints.bytes_per_row_divisor().has_value()
                                                     ? constraints.bytes_per_row_divisor().value()
                                                     : 1;

    const auto& tiling_data = GetTilingData(GetTilingTypeForPixelFormat(pixel_format_and_modifier));

    constraints_bytes_per_row_divisor =
        fbl::round_up(constraints_bytes_per_row_divisor, tiling_data.bytes_per_row_per_tile);

    // This code should match the code in garnet/public/rust/fuchsia-framebuffer/src/sysmem.rs.
    uint32_t non_padding_bytes_per_row;
    if (!CheckMul(ImageFormatStrideBytesPerWidthPixel(pixel_format_and_modifier), width)
             .AssignIfValid(&non_padding_bytes_per_row)) {
      return false;
    }

    *minimum_row_bytes_out =
        fbl::round_up(std::max(non_padding_bytes_per_row, constraints_min_bytes_per_row),
                      constraints_bytes_per_row_divisor);
    if (constraints.max_bytes_per_row().has_value() &&
        *minimum_row_bytes_out > constraints.max_bytes_per_row().value()) {
      return false;
    }
    return true;
  }

 private:
  struct TilingData {
    uint32_t tile_rows;
    uint32_t bytes_per_row_per_tile;
  };

  // These are base Intel tilings, with no aux buffers.
  enum class TilingType { kX, kY, kYf };

  // See
  // https://01.org/sites/default/files/documentation/intel-gfx-prm-osrc-skl-vol05-memory_views.pdf
  static constexpr uint32_t kIntelTileByteSize = 4096;
  static constexpr TilingData kTilingData[] = {
      {
          // kX
          .tile_rows = 8,
          .bytes_per_row_per_tile = 512,
      },
      {
          // kY
          .tile_rows = 32,
          .bytes_per_row_per_tile = 128,
      },
      {
          // kYf
          .tile_rows = 32,
          .bytes_per_row_per_tile = 128,
      },
  };

  // For simplicity CCS plane is always 3, leaving room for Y, U, and V planes if the format is I420
  // or similar.
  static constexpr uint32_t kCcsPlane = 3;

  // See https://01.org/sites/default/files/documentation/intel-gfx-prm-osrc-kbl-vol12-display.pdf
  // for a description of the color control surface. The CCS is always Y-tiled. A CCS cache-line
  // (64 bytes, so 2 fit horizontally in a tile) represents 16 horizontal cache line pairs (so 16
  // tiles) and 16 pixels tall.
  static constexpr uint32_t kCcsTileWidthRatio = 2 * 16;
  static constexpr uint32_t kCcsTileHeightRatio = 16;

  static TilingType GetTilingTypeForPixelFormat(const PixelFormatAndModifier& pixel_format) {
    switch (pixel_format.pixel_format_modifier & ~fuchsia_images2::kFormatModifierIntelCcsBit) {
      case fuchsia_images2::kFormatModifierIntelI915XTiled:
        return TilingType::kX;

      case fuchsia_images2::kFormatModifierIntelI915YTiled:
        return TilingType::kY;

      case fuchsia_images2::kFormatModifierIntelI915YfTiled:
        return TilingType::kYf;
      default:
        ZX_DEBUG_ASSERT(false);
        return TilingType::kX;
    }
  }

  static const TilingData& GetTilingData(TilingType type) {
    static_assert(static_cast<size_t>(TilingType::kYf) < std::size(kTilingData));
    ZX_DEBUG_ASSERT(static_cast<uint32_t>(type) < std::size(kTilingData));
    return kTilingData[static_cast<uint32_t>(type)];
  }

  // Gets the total size (in tiles) of image data for non-aux planes
  static void GetSizeInTiles(const ImageFormat& image_format, uint32_t plane, uint32_t* width_out,
                             uint32_t* height_out) {
    auto pixel_format_and_modifier = PixelFormatAndModifierFromImageFormat(image_format);
    const auto& tiling_data = GetTilingData(GetTilingTypeForPixelFormat(pixel_format_and_modifier));
    ZX_DEBUG_ASSERT(image_format.bytes_per_row().has_value());
    uint32_t bytes_per_row = image_format.bytes_per_row().value();

    const auto& bytes_per_row_per_tile = tiling_data.bytes_per_row_per_tile;
    const auto& tile_rows = tiling_data.tile_rows;

    switch (pixel_format_and_modifier.pixel_format) {
      case PixelFormat::kR8G8B8A8:
      case PixelFormat::kBgra32: {
        // Format only has one plane
        ZX_DEBUG_ASSERT(plane == 0);

        *width_out = fbl::round_up(bytes_per_row, bytes_per_row_per_tile) / bytes_per_row_per_tile;
        *height_out = fbl::round_up(image_format.size()->height(), tile_rows) / tile_rows;
      } break;
      // Since NV12 is a biplanar format we must handle the size for each plane separately. From
      // https://github.com/intel/gmmlib/blob/e1f634c5d5a41ac48756b25697ea499605711747/Source/GmmLib/Texture/GmmTextureAlloc.cpp#L1192:
      // "For Tiled Planar surfaces, the planes must be tile-boundary aligned." Meaning that each
      // plane must be separately tiled aligned.
      case PixelFormat::kNv12:
        if (plane == 0) {
          // Calculate the Y plane size (8 bpp)
          *width_out =
              fbl::round_up(bytes_per_row, bytes_per_row_per_tile) / bytes_per_row_per_tile;
          *height_out = fbl::round_up(image_format.size()->height(), tile_rows) / tile_rows;
        } else if (plane == 1) {
          // Calculate the UV plane size (4 bpp)
          // We effectively have 1/2 the height of our original image since we are subsampled at
          // 4:2:0. Since width of the Y plane must match the width of the UV plane we divide the
          // height of the Y plane by 2 to calculate the height of the UV plane (aligned on tile
          // height boundaries). Ensure the height is aligned 2 before dividing.
          uint32_t adjusted_height = fbl::round_up(image_format.size()->height(), 2u) / 2u;

          *width_out =
              fbl::round_up(bytes_per_row, bytes_per_row_per_tile) / bytes_per_row_per_tile;
          *height_out = fbl::round_up(adjusted_height, tile_rows) / tile_rows;
        } else {
          ZX_DEBUG_ASSERT(false);
        }
        break;
      default:
        ZX_DEBUG_ASSERT(false);
        return;
    }
  }

  static bool FormatHasCcs(const PixelFormatAndModifier& pixel_format) {
    return pixel_format.pixel_format_modifier & fuchsia_images2::kFormatModifierIntelCcsBit;
  }

  // Does not include aux planes
  static uint32_t FormatNumOfPlanes(const PixelFormat& pixel_format) {
    switch (pixel_format) {
      case PixelFormat::kR8G8B8A8:
      case PixelFormat::kBgra32:
        return 1u;
      case PixelFormat::kNv12:
        return 2u;
      default:
        ZX_DEBUG_ASSERT(false);
        return 0u;
    }
  }

  static uint64_t CcsWidthInTiles(uint32_t main_plane_width_in_tiles) {
    return fbl::round_up(main_plane_width_in_tiles, kCcsTileWidthRatio) / kCcsTileWidthRatio;
  }

  static uint64_t CcsSize(uint32_t width_in_tiles, uint32_t height_in_tiles) {
    uint32_t height_in_ccs_tiles =
        fbl::round_up(height_in_tiles, kCcsTileHeightRatio) / kCcsTileHeightRatio;
    return CcsWidthInTiles(width_in_tiles) * height_in_ccs_tiles * kIntelTileByteSize;
  }
};
class AfbcFormats : public ImageFormatSet {
 public:
  const char* Name() const override { return "AfbcFormats"; }

  static constexpr uint64_t kAfbcModifierMask =
      fuchsia_images2::kFormatModifierArmTeBit | fuchsia_images2::kFormatModifierArmSplitBlockBit |
      fuchsia_images2::kFormatModifierArmSparseBit | fuchsia_images2::kFormatModifierArmYuvBit |
      fuchsia_images2::kFormatModifierArmBchBit | fuchsia_images2::kFormatModifierArmTiledHeaderBit;
  bool IsSupported(const PixelFormatAndModifier& pixel_format) const override {
    if (pixel_format.pixel_format != PixelFormatWire::kR8G8B8A8 &&
        pixel_format.pixel_format != PixelFormatWire::kBgra32) {
      return false;
    }
    switch (pixel_format.pixel_format_modifier & ~kAfbcModifierMask) {
      case fuchsia_images2::kFormatModifierArmAfbc16X16:
      case fuchsia_images2::kFormatModifierArmAfbc32X8:
        return true;
      default:
        return false;
    }
  }

  // Calculate the size of the Raw AFBC image without a transaction elimination buffer.
  uint64_t NonTESize(const ImageFormat& image_format) const {
    // See
    // https://android.googlesource.com/device/linaro/hikey/+/android-o-preview-3/gralloc960/alloc_device.cpp
    constexpr uint32_t kAfbcBodyAlignment = 1024u;
    constexpr uint32_t kTiledAfbcBodyAlignment = 4096u;

    ZX_DEBUG_ASSERT(image_format.pixel_format().has_value());
    ZX_DEBUG_ASSERT(image_format.pixel_format_modifier().has_value());
    ZX_DEBUG_ASSERT(IsSupported(PixelFormatAndModifier(
        image_format.pixel_format().value(), image_format.pixel_format_modifier().value())));
    uint32_t block_width;
    uint32_t block_height;
    uint32_t width_alignment;
    uint32_t height_alignment;
    bool tiled_header = image_format.pixel_format_modifier().value() &
                        fuchsia_images2::kFormatModifierArmTiledHeaderBit;

    switch (image_format.pixel_format_modifier().value() & ~kAfbcModifierMask) {
      case fuchsia_images2::kFormatModifierArmAfbc16X16:
        block_width = 16;
        block_height = 16;
        if (!tiled_header) {
          width_alignment = block_width;
          height_alignment = block_height;
        } else {
          width_alignment = 128;
          height_alignment = 128;
        }
        break;

      case fuchsia_images2::kFormatModifierArmAfbc32X8:
        block_width = 32;
        block_height = 8;
        if (!tiled_header) {
          width_alignment = block_width;
          height_alignment = block_height;
        } else {
          width_alignment = 256;
          height_alignment = 64;
        }
        break;
      default:
        return 0;
    }

    uint32_t body_alignment = tiled_header ? kTiledAfbcBodyAlignment : kAfbcBodyAlignment;

    ZX_DEBUG_ASSERT(image_format.pixel_format().has_value());
    ZX_DEBUG_ASSERT(image_format.pixel_format().value() == PixelFormatWire::kR8G8B8A8 ||
                    image_format.pixel_format().value() == PixelFormatWire::kBgra32);
    constexpr uint32_t kBytesPerPixel = 4;
    constexpr uint32_t kBytesPerBlockHeader = 16;

    ZX_DEBUG_ASSERT(image_format.size().has_value());
    uint64_t block_count =
        fbl::round_up(image_format.size()->width(), width_alignment) / block_width *
        fbl::round_up(image_format.size()->height(), height_alignment) / block_height;
    return block_count * block_width * block_height * kBytesPerPixel +
           fbl::round_up(block_count * kBytesPerBlockHeader, body_alignment);
  }

  uint64_t ImageFormatImageSize(const ImageFormat& image_format) const override {
    uint64_t size = NonTESize(image_format);
    if (image_format.pixel_format_modifier().value() & fuchsia_images2::kFormatModifierArmTeBit) {
      size += arm_transaction_elimination_buffer_size(size, image_format.size()->width(),
                                                      image_format.size()->height());
    }

    return size;
  }

  bool ImageFormatPlaneByteOffset(const ImageFormat& image_format, uint32_t plane,
                                  uint64_t* offset_out) const override {
    ZX_DEBUG_ASSERT(IsSupported(PixelFormatAndModifier(
        image_format.pixel_format().value(), image_format.pixel_format_modifier().value())));
    if (plane == 0) {
      *offset_out = 0;
      return true;
    }
    if (plane == kTransactionEliminationPlane) {
      *offset_out = fbl::round_up(NonTESize(image_format), kTransactionEliminationAlignment);
      return true;
    }
    return false;
  }
  bool ImageFormatPlaneRowBytes(const ImageFormat& image_format, uint32_t plane,
                                uint32_t* row_bytes_out) const override {
    if (plane == 0) {
      *row_bytes_out = 0;
      return true;
    }
    if (plane == kTransactionEliminationPlane) {
      *row_bytes_out = arm_transaction_elimination_row_size(image_format.size()->width());
      return true;
    }
    return false;
  }
  bool ImageFormatMinimumRowBytes(const fuchsia_sysmem2::ImageFormatConstraints& constraints,
                                  uint32_t width, uint32_t* minimum_row_bytes_out) const override {
    ZX_DEBUG_ASSERT(IsSupported(PixelFormatAndModifier(
        constraints.pixel_format().value(), constraints.pixel_format_modifier().value())));

    if (constraints.min_size().has_value() && width < constraints.min_size()->width()) {
      return false;
    }
    if (constraints.max_size().has_value() && width > constraints.max_size()->width()) {
      return false;
    }

    uint32_t block_width;
    uint32_t width_alignment;
    bool tiled_header = constraints.pixel_format_modifier().value() &
                        fuchsia_images2::kFormatModifierArmTiledHeaderBit;

    switch (constraints.pixel_format_modifier().value() & ~kAfbcModifierMask) {
      case fuchsia_images2::kFormatModifierArmAfbc16X16:
        block_width = 16;
        if (!tiled_header) {
          width_alignment = block_width;
        } else {
          width_alignment = 128;
        }
        break;

      case fuchsia_images2::kFormatModifierArmAfbc32X8:
        block_width = 32;
        if (!tiled_header) {
          width_alignment = block_width;
        } else {
          width_alignment = 256;
        }
        break;
      default:
        return false;
    }

    // Divide with round up instead of down: (width + (width_alignment - 1)) / width_alignment
    auto width_in_blocks = CheckDiv(CheckAdd(width, CheckSub(width_alignment, 1)), width_alignment);
    auto width_in_pixels = CheckMul(width_in_blocks, block_width);
    constexpr uint32_t kBytesPerPixel = 4;
    auto width_in_bytes = CheckMul(width_in_pixels, kBytesPerPixel);
    if (!width_in_bytes.AssignIfValid(minimum_row_bytes_out)) {
      return false;
    }

    return true;
  }
};

uint64_t linear_size(uint32_t surface_height, uint32_t bytes_per_row, PixelFormat type) {
  switch (type) {
    case PixelFormat::kR8G8B8A8:
    case PixelFormat::kBgra32:
    case PixelFormat::kBgr24:
    case PixelFormat::kRgb565:
    case PixelFormat::kRgb332:
    case PixelFormat::kRgb2220:
    case PixelFormat::kL8:
    case PixelFormat::kR8:
    case PixelFormat::kR8G8:
    case PixelFormat::kA2B10G10R10:
    case PixelFormat::kA2R10G10B10:
      return surface_height * bytes_per_row;
    case PixelFormat::kI420:
      return surface_height * bytes_per_row * 3 / 2;
    case PixelFormat::kM420:
      return surface_height * bytes_per_row * 3 / 2;
    case PixelFormat::kNv12:
      return surface_height * bytes_per_row * 3 / 2;
    case PixelFormat::kYuy2:
      return surface_height * bytes_per_row;
    case PixelFormat::kYv12:
      return surface_height * bytes_per_row * 3 / 2;
    default:
      return 0u;
  }
}

bool linear_minimum_row_bytes(const fuchsia_sysmem2::ImageFormatConstraints& constraints,
                              uint32_t width, uint32_t* minimum_row_bytes_out) {
  ZX_DEBUG_ASSERT(minimum_row_bytes_out);
  // Caller must set pixel_format.
  ZX_DEBUG_ASSERT(constraints.pixel_format().has_value());

  if ((constraints.min_size().has_value() && width < constraints.min_size()->width()) ||
      (constraints.max_size().has_value() && width > constraints.max_size()->width())) {
    return false;
  }

  // We don't enforce that width is already aligned up by the caller by the time of this call, but
  // if the caller is a producer, the caller is nonetheless expected to conform to the
  // size_alignment.width divisibility requirement for any fuchsia.sysmem.ImageFormat2.coded_width
  // or fuchsia.images2.ImageFormat.size.width values the caller generates.
  if (constraints.size_alignment().has_value()) {
    width = fbl::round_up(width, constraints.size_alignment()->width());
  }

  uint32_t constraints_min_bytes_per_row =
      constraints.min_bytes_per_row().has_value() ? constraints.min_bytes_per_row().value() : 0;
  uint32_t constraints_bytes_per_row_divisor = constraints.bytes_per_row_divisor().has_value()
                                                   ? constraints.bytes_per_row_divisor().value()
                                                   : 1;

  // This code should match the code in garnet/public/rust/fuchsia-framebuffer/src/sysmem.rs.
  *minimum_row_bytes_out = fbl::round_up(
      std::max(
          ImageFormatStrideBytesPerWidthPixel(PixelFormatAndModifierFromConstraints(constraints)) *
              width,
          constraints_min_bytes_per_row),
      constraints_bytes_per_row_divisor);
  if (constraints.max_bytes_per_row().has_value() &&
      *minimum_row_bytes_out > constraints.max_bytes_per_row().value()) {
    return false;
  }
  return true;
}

class LinearFormats : public ImageFormatSet {
 public:
  const char* Name() const override { return "LinearFormats"; }

  bool IsSupported(const PixelFormatAndModifier& pixel_format) const override {
    if (pixel_format.pixel_format_modifier != fuchsia_images2::kFormatModifierLinear) {
      return false;
    }
    switch (pixel_format.pixel_format) {
      case PixelFormat::kInvalid:
      case PixelFormat::kDoNotCare:
      case PixelFormat::kMjpeg:
        return false;
      case PixelFormat::kR8G8B8A8:
      case PixelFormat::kBgra32:
      case PixelFormat::kBgr24:
      case PixelFormat::kI420:
      case PixelFormat::kM420:
      case PixelFormat::kNv12:
      case PixelFormat::kYuy2:
      case PixelFormat::kYv12:
      case PixelFormat::kRgb565:
      case PixelFormat::kRgb332:
      case PixelFormat::kRgb2220:
      case PixelFormat::kL8:
      case PixelFormat::kR8:
      case PixelFormat::kR8G8:
      case PixelFormat::kA2B10G10R10:
      case PixelFormat::kA2R10G10B10:
        return true;
      default:
        return false;
    }
    return false;
  }

  uint64_t ImageFormatImageSize(const ImageFormat& image_format) const override {
    ZX_DEBUG_ASSERT(image_format.pixel_format().has_value());
    auto pixel_format_and_modifier = PixelFormatAndModifierFromImageFormat(image_format);
    ZX_DEBUG_ASSERT(IsSupported(pixel_format_and_modifier));
    ZX_DEBUG_ASSERT(image_format.size().has_value());
    ZX_DEBUG_ASSERT(image_format.bytes_per_row().has_value());
    uint32_t surface_height = image_format.size()->height();
    uint32_t bytes_per_row =
        image_format.bytes_per_row().has_value() ? image_format.bytes_per_row().value() : 0;
    return linear_size(surface_height, bytes_per_row, pixel_format_and_modifier.pixel_format);
  }

  bool ImageFormatPlaneByteOffset(const ImageFormat& image_format, uint32_t plane,
                                  uint64_t* offset_out) const override {
    if (plane == 0) {
      *offset_out = 0;
      return true;
    }
    if (plane == 1) {
      switch (image_format.pixel_format().value()) {
        case PixelFormat::kNv12:
        case PixelFormat::kI420:
        case PixelFormat::kYv12: {
          *offset_out =
              CheckMul(image_format.size().value().height(), image_format.bytes_per_row().value())
                  .ValueOrDie();
          return true;
        }
        default:
          return false;
      }
    }
    if (plane == 2) {
      switch (image_format.pixel_format().value()) {
        case PixelFormat::kI420:
        case PixelFormat::kYv12: {
          auto luma_bytes =
              CheckMul(image_format.size().value().height(), image_format.bytes_per_row().value());
          auto one_chroma_plane_bytes = CheckMul(CheckDiv(image_format.size().value().height(), 2),
                                                 CheckDiv(image_format.bytes_per_row().value(), 2));
          auto offset_just_past_luma_and_one_chroma = CheckAdd(luma_bytes, one_chroma_plane_bytes);
          // 2nd chroma plane is just past luma and 1st chroma plane
          *offset_out = offset_just_past_luma_and_one_chroma.ValueOrDie();
          return true;
        }
        default:
          return false;
      }
    }

    return false;
  }

  bool ImageFormatPlaneRowBytes(const ImageFormat& image_format, uint32_t plane,
                                uint32_t* row_bytes_out) const override {
    auto pixel_format_and_modifier = PixelFormatAndModifierFromImageFormat(image_format);
    if (plane == 0) {
      *row_bytes_out = image_format.bytes_per_row().value();
      return true;
    }
    if (plane == 1) {
      switch (pixel_format_and_modifier.pixel_format) {
        case PixelFormat::kNv12:
          *row_bytes_out = image_format.bytes_per_row().value();
          return true;
        case PixelFormat::kI420:
        case PixelFormat::kYv12:
          *row_bytes_out = image_format.bytes_per_row().value() / 2;
          return true;
        default:
          return false;
      }
    } else if (plane == 2) {
      switch (pixel_format_and_modifier.pixel_format) {
        case PixelFormat::kI420:
        case PixelFormat::kYv12:
          *row_bytes_out = image_format.bytes_per_row().value() / 2;
          return true;
        default:
          return false;
      }
    }
    return false;
  }

  bool ImageFormatMinimumRowBytes(const fuchsia_sysmem2::ImageFormatConstraints& constraints,
                                  uint32_t width, uint32_t* minimum_row_bytes_out) const override {
    return linear_minimum_row_bytes(constraints, width, minimum_row_bytes_out);
  }
};

constexpr LinearFormats kLinearFormats;

class GoldfishFormats : public ImageFormatSet {
 public:
  const char* Name() const override { return "GoldfishFormats"; }

  bool IsSupported(const PixelFormatAndModifier& pixel_format) const override {
    switch (pixel_format.pixel_format_modifier) {
      case fuchsia_images2::kFormatModifierGoogleGoldfishOptimal:
        return true;
      default:
        return false;
    }
  }
  uint64_t ImageFormatImageSize(const ImageFormat& image_format) const override {
    ZX_DEBUG_ASSERT(image_format.pixel_format().has_value());
    auto pixel_format_and_modifier = PixelFormatAndModifierFromImageFormat(image_format);
    ZX_DEBUG_ASSERT(IsSupported(pixel_format_and_modifier));
    ZX_DEBUG_ASSERT(image_format.size().has_value());
    ZX_DEBUG_ASSERT(image_format.bytes_per_row().has_value());

    uint32_t surface_height = image_format.size()->height();
    uint32_t bytes_per_row = image_format.bytes_per_row().value();
    return linear_size(surface_height, bytes_per_row, image_format.pixel_format().value());
  }
  bool ImageFormatPlaneByteOffset(const ImageFormat& image_format, uint32_t plane,
                                  uint64_t* offset_out) const override {
    ZX_DEBUG_ASSERT(IsSupported(PixelFormatAndModifier(
        image_format.pixel_format().value(), image_format.pixel_format_modifier().value())));
    if (plane == 0) {
      *offset_out = 0;
      return true;
    }
    return false;
  }
  bool ImageFormatPlaneRowBytes(const ImageFormat& image_format, uint32_t plane,
                                uint32_t* row_bytes_out) const override {
    if (plane == 0) {
      *row_bytes_out = image_format.bytes_per_row().value();
      return true;
    }
    return false;
  }
  bool ImageFormatMinimumRowBytes(const fuchsia_sysmem2::ImageFormatConstraints& constraints,
                                  uint32_t width, uint32_t* minimum_row_bytes_out) const override {
    return false;
  }
};

class ArmTELinearFormats : public ImageFormatSet {
 public:
  const char* Name() const override { return "ArmTELinearFormats"; }

  bool IsSupported(const PixelFormatAndModifier& pixel_format) const override {
    if (pixel_format.pixel_format_modifier != fuchsia_images2::kFormatModifierArmLinearTe)
      return false;
    switch (pixel_format.pixel_format) {
      case PixelFormat::kInvalid:
      case PixelFormat::kDoNotCare:
      case PixelFormat::kMjpeg:
        return false;
      case PixelFormat::kR8G8B8A8:
      case PixelFormat::kBgra32:
      case PixelFormat::kBgr24:
      case PixelFormat::kI420:
      case PixelFormat::kM420:
      case PixelFormat::kNv12:
      case PixelFormat::kYuy2:
      case PixelFormat::kYv12:
      case PixelFormat::kRgb565:
      case PixelFormat::kRgb332:
      case PixelFormat::kRgb2220:
      case PixelFormat::kL8:
      case PixelFormat::kR8:
      case PixelFormat::kR8G8:
      case PixelFormat::kA2B10G10R10:
      case PixelFormat::kA2R10G10B10:
        return true;
      default:
        return false;
    }
    return false;
  }

  uint64_t ImageFormatImageSize(const fuchsia_images2::ImageFormat& image_format) const override {
    auto pixel_format_and_modifier = PixelFormatAndModifierFromImageFormat(image_format);
    ZX_DEBUG_ASSERT(IsSupported(pixel_format_and_modifier));
    ZX_DEBUG_ASSERT(image_format.size().has_value());
    ZX_DEBUG_ASSERT(image_format.bytes_per_row().has_value());
    uint32_t bytes_per_row = image_format.bytes_per_row().value();
    uint64_t size = linear_size(image_format.size()->height(), bytes_per_row,
                                pixel_format_and_modifier.pixel_format);
    uint64_t crc_size = arm_transaction_elimination_buffer_size(size, image_format.size()->width(),
                                                                image_format.size()->height());
    return size + crc_size;
  }

  bool ImageFormatPlaneByteOffset(const fuchsia_images2::ImageFormat& image_format, uint32_t plane,
                                  uint64_t* offset_out) const override {
    if (plane < kTransactionEliminationPlane) {
      return kLinearFormats.ImageFormatPlaneByteOffset(image_format, plane, offset_out);
    }
    if (plane == kTransactionEliminationPlane) {
      ZX_DEBUG_ASSERT(image_format.size().has_value());
      ZX_DEBUG_ASSERT(image_format.bytes_per_row().has_value());
      uint32_t bytes_per_row = image_format.bytes_per_row().value();
      uint64_t size = linear_size(image_format.size()->height(), bytes_per_row,
                                  image_format.pixel_format().value());
      *offset_out = fbl::round_up(size, 64u);
      return true;
    }

    return false;
  }

  bool ImageFormatPlaneRowBytes(const fuchsia_images2::ImageFormat& image_format, uint32_t plane,
                                uint32_t* row_bytes_out) const override {
    if (plane < kTransactionEliminationPlane) {
      return kLinearFormats.ImageFormatPlaneRowBytes(image_format, plane, row_bytes_out);
    }
    if (plane == kTransactionEliminationPlane) {
      if (!image_format.size().has_value()) {
        return false;
      }
      *row_bytes_out = arm_transaction_elimination_row_size(image_format.size()->width());
      return true;
    }
    return false;
  }

  bool ImageFormatMinimumRowBytes(const fuchsia_sysmem2::ImageFormatConstraints& constraints,
                                  uint32_t width, uint32_t* minimum_row_bytes_out) const override {
    return linear_minimum_row_bytes(constraints, width, minimum_row_bytes_out);
  }
};

constexpr IntelTiledFormats kIntelFormats;
constexpr AfbcFormats kAfbcFormats;
constexpr ArmTELinearFormats kArmTELinearFormats;
constexpr GoldfishFormats kGoldfishFormats;

constexpr const ImageFormatSet* kImageFormats[] = {
    &kLinearFormats, &kIntelFormats, &kAfbcFormats, &kArmTELinearFormats, &kGoldfishFormats,
};

}  // namespace

bool ImageFormatIsPixelFormatEqual(const PixelFormatAndModifier& a,
                                   const PixelFormatAndModifier& b) {
  if (a.pixel_format != b.pixel_format) {
    return false;
  }
  if (a.pixel_format_modifier != b.pixel_format_modifier) {
    return false;
  }
  return true;
}

bool ImageFormatIsPixelFormatEqual(const fuchsia_sysmem::wire::PixelFormat& wire_a_v1,
                                   const fuchsia_sysmem::wire::PixelFormat& wire_b_v1) {
  auto a_v1 = fidl::ToNatural(wire_a_v1);
  auto b_v1 = fidl::ToNatural(wire_b_v1);
  auto a_v2 = sysmem::V2CopyFromV1PixelFormat(a_v1);
  auto b_v2 = sysmem::V2CopyFromV1PixelFormat(b_v1);
  return ImageFormatIsPixelFormatEqual(a_v2, b_v2);
}

bool ImageFormatIsSupportedColorSpaceForPixelFormat(const fuchsia_images2::ColorSpace& color_space,
                                                    const PixelFormatAndModifier& pixel_format) {
  if (color_space == ColorSpace::kPassthrough) {
    return true;
  }
  // Ignore pixel format modifier - assume it has already been checked.
  auto color_space_sampling_info_iter = kColorSpaceSamplingInfo.find(color_space);
  if (color_space_sampling_info_iter == kColorSpaceSamplingInfo.end()) {
    return false;
  }
  auto pixel_format_sampling_info_iter = kPixelFormatSamplingInfo.find(pixel_format.pixel_format);
  if (pixel_format_sampling_info_iter == kPixelFormatSamplingInfo.end()) {
    return false;
  }
  const SamplingInfo& color_space_sampling_info = color_space_sampling_info_iter->second;
  const SamplingInfo& pixel_format_sampling_info = pixel_format_sampling_info_iter->second;
  if (color_space_sampling_info.color_type != pixel_format_sampling_info.color_type) {
    return false;
  }
  bool is_bits_per_sample_match_found = false;
  for (uint32_t bits_per_sample : color_space_sampling_info.possible_bits_per_sample) {
    auto pixel_format_bits_per_sample_iter =
        pixel_format_sampling_info.possible_bits_per_sample.find(bits_per_sample);
    if (pixel_format_bits_per_sample_iter !=
        pixel_format_sampling_info.possible_bits_per_sample.end()) {
      is_bits_per_sample_match_found = true;
      break;
    }
  }
  return is_bits_per_sample_match_found;
}

bool ImageFormatIsSupportedColorSpaceForPixelFormat(
    const fuchsia_sysmem::wire::ColorSpace& wire_color_space_v1,
    const fuchsia_sysmem::wire::PixelFormat& wire_pixel_format_v1) {
  auto color_space_v1 = fidl::ToNatural(wire_color_space_v1);
  auto pixel_format_v1 = fidl::ToNatural(wire_pixel_format_v1);
  auto color_space_v2 = sysmem::V2CopyFromV1ColorSpace(color_space_v1);
  auto pixel_format_v2 = sysmem::V2CopyFromV1PixelFormat(pixel_format_v1);
  return ImageFormatIsSupportedColorSpaceForPixelFormat(color_space_v2, pixel_format_v2);
}

bool ImageFormatIsSupported(const PixelFormatAndModifier& pixel_format) {
  return std::any_of(std::begin(kImageFormats), std::end(kImageFormats),
                     [pixel_format](const ImageFormatSet* format_set) {
                       return format_set->IsSupported(pixel_format);
                     });
}

bool ImageFormatIsSupported(const fuchsia_sysmem::wire::PixelFormat& wire_pixel_format_v1) {
  auto pixel_format_v1 = fidl::ToNatural(wire_pixel_format_v1);
  auto pixel_format_v2 = sysmem::V2CopyFromV1PixelFormat(pixel_format_v1);
  return ImageFormatIsSupported(pixel_format_v2);
}

uint32_t ImageFormatBitsPerPixel(const PixelFormatAndModifier& pixel_format) {
  ZX_DEBUG_ASSERT(ImageFormatIsSupported(pixel_format));
  switch (pixel_format.pixel_format) {
    case PixelFormat::kInvalid:
    case PixelFormat::kDoNotCare:
    case PixelFormat::kMjpeg:
      // impossible; checked previously.
      ZX_DEBUG_ASSERT(false);
      return 0u;
    case PixelFormat::kR8G8B8A8:
      return 4u * 8u;
    case PixelFormat::kBgra32:
      return 4u * 8u;
    case PixelFormat::kBgr24:
      return 3u * 8u;
    case PixelFormat::kI420:
      return 12u;
    case PixelFormat::kM420:
      return 12u;
    case PixelFormat::kNv12:
      return 12u;
    case PixelFormat::kYuy2:
      return 2u * 8u;
    case PixelFormat::kYv12:
      return 12u;
    case PixelFormat::kRgb565:
      return 16u;
    case PixelFormat::kRgb332:
    case PixelFormat::kRgb2220:
    case PixelFormat::kL8:
    case PixelFormat::kR8:
      return 8u;
    case PixelFormat::kR8G8:
      return 16u;
    case PixelFormat::kA2B10G10R10:
    case PixelFormat::kA2R10G10B10:
      return 2u + 3 * 10u;
    default:
      ZX_PANIC("Unknown Pixel Format: %u", sysmem::fidl_underlying_cast(pixel_format.pixel_format));
  }
}

uint32_t ImageFormatBitsPerPixel(const fuchsia_sysmem::wire::PixelFormat& wire_pixel_format_v1) {
  auto pixel_format_v1 = fidl::ToNatural(wire_pixel_format_v1);
  auto pixel_format_v2 = sysmem::V2CopyFromV1PixelFormat(pixel_format_v1);
  return ImageFormatBitsPerPixel(pixel_format_v2);
}

uint32_t ImageFormatStrideBytesPerWidthPixel(const PixelFormatAndModifier& pixel_format) {
  ZX_DEBUG_ASSERT(ImageFormatIsSupported(pixel_format));
  // This list should match the one in garnet/public/rust/fuchsia-framebuffer/src/sysmem.rs.
  switch (pixel_format.pixel_format) {
    case PixelFormat::kInvalid:
    case PixelFormat::kDoNotCare:
    case PixelFormat::kMjpeg:
      // impossible; checked previously.
      ZX_DEBUG_ASSERT(false);
      return 0u;
    case PixelFormat::kR8G8B8A8:
      return 4u;
    case PixelFormat::kBgra32:
      return 4u;
    case PixelFormat::kBgr24:
      return 3u;
    case PixelFormat::kI420:
      return 1u;
    case PixelFormat::kM420:
      return 1u;
    case PixelFormat::kNv12:
      return 1u;
    case PixelFormat::kYuy2:
      return 2u;
    case PixelFormat::kYv12:
      return 1u;
    case PixelFormat::kRgb565:
      return 2u;
    case PixelFormat::kRgb332:
      return 1u;
    case PixelFormat::kRgb2220:
      return 1u;
    case PixelFormat::kL8:
      return 1u;
    case PixelFormat::kR8:
      return 1u;
    case PixelFormat::kR8G8:
      return 2u;
    case PixelFormat::kA2B10G10R10:
      return 4u;
    case PixelFormat::kA2R10G10B10:
      return 4u;
    default:
      ZX_PANIC("Unknown Pixel Format: %u", sysmem::fidl_underlying_cast(pixel_format.pixel_format));
  }
}

uint32_t ImageFormatStrideBytesPerWidthPixel(
    const fuchsia_sysmem::wire::PixelFormat& wire_pixel_format_v1) {
  auto pixel_format_v1 = fidl::ToNatural(wire_pixel_format_v1);
  auto pixel_format_v2 = sysmem::V2CopyFromV1PixelFormat(pixel_format_v1);
  return ImageFormatStrideBytesPerWidthPixel(pixel_format_v2);
}

uint64_t ImageFormatImageSize(const fuchsia_images2::ImageFormat& image_format) {
  ZX_DEBUG_ASSERT(image_format.pixel_format().has_value());
  auto pixel_format_and_modifier = PixelFormatAndModifierFromImageFormat(image_format);
  for (auto format_set : kImageFormats) {
    if (format_set->IsSupported(pixel_format_and_modifier)) {
      return format_set->ImageFormatImageSize(image_format);
    }
  }
  ZX_PANIC("Unknown Pixel Format: %u",
           sysmem::fidl_underlying_cast(image_format.pixel_format().value()));
  return 0;
}

uint64_t ImageFormatImageSize(const fuchsia_images2::wire::ImageFormat& image_format) {
  return ImageFormatImageSize(fidl::ToNatural(image_format));
}

uint64_t ImageFormatImageSize(const fuchsia_sysmem::wire::ImageFormat2& wire_image_format_v1) {
  auto image_format_v1 = fidl::ToNatural(wire_image_format_v1);
  auto image_format_v2 = sysmem::V2CopyFromV1ImageFormat(image_format_v1).take_value();
  return ImageFormatImageSize(image_format_v2);
}

uint32_t ImageFormatSurfaceWidthMinDivisor(const PixelFormatAndModifier& pixel_format) {
  ZX_DEBUG_ASSERT(ImageFormatIsSupported(pixel_format));
  switch (pixel_format.pixel_format) {
    case PixelFormat::kInvalid:
    case PixelFormat::kDoNotCare:
    case PixelFormat::kMjpeg:
      // impossible; checked previously.
      ZX_DEBUG_ASSERT(false);
      return 0u;
    case PixelFormat::kR8G8B8A8:
      return 1u;
    case PixelFormat::kBgra32:
      return 1u;
    case PixelFormat::kBgr24:
      return 1u;
    case PixelFormat::kI420:
      return 2u;
    case PixelFormat::kM420:
      return 2u;
    case PixelFormat::kNv12:
      return 2u;
    case PixelFormat::kYuy2:
      return 2u;
    case PixelFormat::kYv12:
      return 2u;
    case PixelFormat::kRgb565:
      return 1u;
    case PixelFormat::kRgb332:
      return 1u;
    case PixelFormat::kRgb2220:
      return 1u;
    case PixelFormat::kL8:
      return 1u;
    case PixelFormat::kR8:
      return 1u;
    case PixelFormat::kR8G8:
      return 1u;
    case PixelFormat::kA2B10G10R10:
      return 1u;
    case PixelFormat::kA2R10G10B10:
      return 1u;
    default:
      ZX_PANIC("Unknown Pixel Format: %u", sysmem::fidl_underlying_cast(pixel_format.pixel_format));
  }
}

uint32_t ImageFormatCodedWidthMinDivisor(
    const fuchsia_sysmem::wire::PixelFormat& wire_pixel_format_v1) {
  auto pixel_format_v1 = fidl::ToNatural(wire_pixel_format_v1);
  auto pixel_format_v2 = sysmem::V2CopyFromV1PixelFormat(pixel_format_v1);
  return ImageFormatSurfaceWidthMinDivisor(pixel_format_v2);
}

uint32_t ImageFormatSurfaceHeightMinDivisor(const PixelFormatAndModifier& pixel_format) {
  ZX_DEBUG_ASSERT(ImageFormatIsSupported(pixel_format));
  switch (pixel_format.pixel_format) {
    case PixelFormat::kInvalid:
    case PixelFormat::kDoNotCare:
    case PixelFormat::kMjpeg:
      // impossible; checked previously.
      ZX_DEBUG_ASSERT(false);
      return 0u;
    case PixelFormat::kR8G8B8A8:
      return 1u;
    case PixelFormat::kBgra32:
      return 1u;
    case PixelFormat::kBgr24:
      return 1u;
    case PixelFormat::kI420:
      return 2u;
    case PixelFormat::kM420:
      return 2u;
    case PixelFormat::kNv12:
      return 2u;
    case PixelFormat::kYuy2:
      return 2u;
    case PixelFormat::kYv12:
      return 2u;
    case PixelFormat::kRgb565:
      return 1u;
    case PixelFormat::kRgb332:
      return 1u;
    case PixelFormat::kRgb2220:
      return 1u;
    case PixelFormat::kL8:
      return 1u;
    case PixelFormat::kR8:
      return 1u;
    case PixelFormat::kR8G8:
      return 1u;
    case PixelFormat::kA2B10G10R10:
      return 1u;
    case PixelFormat::kA2R10G10B10:
      return 1u;
    default:
      ZX_PANIC("Unknown Pixel Format: %u", sysmem::fidl_underlying_cast(pixel_format.pixel_format));
  }
}

uint32_t ImageFormatCodedHeightMinDivisor(
    const fuchsia_sysmem::wire::PixelFormat& wire_pixel_format_v1) {
  auto pixel_format_v1 = fidl::ToNatural(wire_pixel_format_v1);
  auto pixel_format_v2 = sysmem::V2CopyFromV1PixelFormat(pixel_format_v1);
  return ImageFormatSurfaceHeightMinDivisor(pixel_format_v2);
}

uint32_t ImageFormatSampleAlignment(const PixelFormatAndModifier& pixel_format) {
  ZX_DEBUG_ASSERT(ImageFormatIsSupported(pixel_format));
  switch (pixel_format.pixel_format) {
    case PixelFormat::kInvalid:
    case PixelFormat::kDoNotCare:
    case PixelFormat::kMjpeg:
      // impossible; checked previously.
      ZX_DEBUG_ASSERT(false);
      return 0u;
    case PixelFormat::kR8G8B8A8:
      return 4u;
    case PixelFormat::kBgra32:
      return 4u;
    case PixelFormat::kBgr24:
      return 1u;
    case PixelFormat::kI420:
      return 2u;
    case PixelFormat::kM420:
      return 2u;
    case PixelFormat::kNv12:
      return 2u;
    case PixelFormat::kYuy2:
      return 2u;
    case PixelFormat::kYv12:
      return 2u;
    case PixelFormat::kRgb565:
      return 2u;
    case PixelFormat::kRgb332:
      return 1u;
    case PixelFormat::kRgb2220:
      return 1u;
    case PixelFormat::kL8:
      return 1u;
    case PixelFormat::kR8:
      return 1u;
    case PixelFormat::kR8G8:
      return 2u;
    case PixelFormat::kA2B10G10R10:
      return 4u;
    case PixelFormat::kA2R10G10B10:
      return 4u;
    default:
      ZX_PANIC("Unknown Pixel Format: %u", sysmem::fidl_underlying_cast(pixel_format.pixel_format));
  }
}

uint32_t ImageFormatSampleAlignment(const fuchsia_sysmem::wire::PixelFormat& wire_pixel_format_v1) {
  auto pixel_format_v1 = fidl::ToNatural(wire_pixel_format_v1);
  auto pixel_format_v2 = sysmem::V2CopyFromV1PixelFormat(pixel_format_v1);
  return ImageFormatSampleAlignment(pixel_format_v2);
}

bool ImageFormatMinimumRowBytes(const fuchsia_sysmem2::ImageFormatConstraints& constraints,
                                uint32_t width, uint32_t* minimum_row_bytes_out) {
  ZX_ASSERT(width != 0);
  ZX_DEBUG_ASSERT(minimum_row_bytes_out);
  // Caller must set pixel_format.
  ZX_DEBUG_ASSERT(constraints.pixel_format().has_value());
  auto pixel_format_and_modifier = PixelFormatAndModifierFromConstraints(constraints);
  for (auto& format_set : kImageFormats) {
    if (format_set->IsSupported(pixel_format_and_modifier)) {
      bool result =
          format_set->ImageFormatMinimumRowBytes(constraints, width, minimum_row_bytes_out);
      ZX_ASSERT(!result || *minimum_row_bytes_out != 0);
      return result;
    }
  }
  return false;
}

bool ImageFormatMinimumRowBytes(
    const fuchsia_sysmem2::wire::ImageFormatConstraints& wire_constraints, uint32_t width,
    uint32_t* minimum_row_bytes_out) {
  return ImageFormatMinimumRowBytes(fidl::ToNatural(wire_constraints), width,
                                    minimum_row_bytes_out);
}

bool ImageFormatMinimumRowBytes(
    const fuchsia_sysmem::wire::ImageFormatConstraints& wire_image_format_constraints_v1,
    uint32_t width, uint32_t* minimum_row_bytes_out) {
  auto image_format_constraints_v1 = fidl::ToNatural(wire_image_format_constraints_v1);
  auto image_format_constraints_v2 =
      sysmem::V2CopyFromV1ImageFormatConstraints(image_format_constraints_v1).take_value();
  return ImageFormatMinimumRowBytes(image_format_constraints_v2, width, minimum_row_bytes_out);
}

fpromise::result<fuchsia_images2::wire::PixelFormat> ImageFormatConvertZbiToSysmemPixelFormat_v2(
    zbi_pixel_format_t zx_pixel_format) {
  switch (zx_pixel_format) {
    case ZBI_PIXEL_FORMAT_RGB_565:
      return fpromise::ok(PixelFormat::kRgb565);
    case ZBI_PIXEL_FORMAT_RGB_332:
      return fpromise::ok(PixelFormat::kRgb332);
    case ZBI_PIXEL_FORMAT_RGB_2220:
      return fpromise::ok(PixelFormat::kRgb2220);
    case ZBI_PIXEL_FORMAT_ARGB_8888:
      // Switching to using alpha.
    case ZBI_PIXEL_FORMAT_RGB_X888:
      return fpromise::ok(PixelFormat::kBgra32);
    case ZBI_PIXEL_FORMAT_MONO_8:
      return fpromise::ok(PixelFormat::kL8);
    case ZBI_PIXEL_FORMAT_I420:
      return fpromise::ok(PixelFormat::kI420);
    case ZBI_PIXEL_FORMAT_NV12:
      return fpromise::ok(PixelFormat::kNv12);
    case ZBI_PIXEL_FORMAT_RGB_888:
      return fpromise::ok(PixelFormat::kBgr24);
    case ZBI_PIXEL_FORMAT_ABGR_8888:
      // Switching to using alpha.
    case ZBI_PIXEL_FORMAT_BGR_888_X:
      return fpromise::ok(PixelFormat::kR8G8B8A8);
    case ZBI_PIXEL_FORMAT_ARGB_2_10_10_10:
      return fpromise::ok(PixelFormat::kA2R10G10B10);
    case ZBI_PIXEL_FORMAT_ABGR_2_10_10_10:
      return fpromise::ok(PixelFormat::kA2B10G10R10);
    default:
      return fpromise::error();
  }
}

fpromise::result<ImageFormat> ImageConstraintsToFormat(const ImageFormatConstraints& constraints,
                                                       uint32_t width, uint32_t height) {
  if ((constraints.min_size().has_value() && height < constraints.min_size()->height()) ||
      (constraints.max_size().has_value() && height > constraints.max_size()->height())) {
    return fpromise::error();
  }
  if ((constraints.min_size().has_value() && width < constraints.min_size()->width()) ||
      (constraints.max_size().has_value() && width > constraints.max_size()->width())) {
    return fpromise::error();
  }
  ImageFormat result;
  uint32_t minimum_row_bytes;
  if (!ImageFormatMinimumRowBytes(constraints, width, &minimum_row_bytes)) {
    return fpromise::error();
  }
  ZX_ASSERT(minimum_row_bytes != 0);
  result.bytes_per_row() = minimum_row_bytes;
  result.pixel_format() = constraints.pixel_format().value();
  result.pixel_format_modifier() = constraints.pixel_format_modifier().value();
  result.size() = {width, height};
  // We intentionally default to x, y == 0, 0 here, since that's by far the most common.  The caller
  // can fix this up if the caller needs non-zero x or non-zero y.
  result.display_rect() = {0, 0, width, height};
  if (constraints.color_spaces().has_value() && !constraints.color_spaces()->empty()) {
    result.color_space() = constraints.color_spaces()->at(0);
  }
  // result's pixel_aspect_ratio field remains un-set which is equivalent to 1,1
  return fpromise::ok(std::move(result));
}

fpromise::result<ImageFormatWire> ImageConstraintsToFormat(
    fidl::AnyArena& allocator, const ImageFormatConstraintsWire& wire_constraints, uint32_t width,
    uint32_t height) {
  auto constraints = fidl::ToNatural(wire_constraints);
  auto result = ImageConstraintsToFormat(constraints, width, height);
  if (!result.is_ok()) {
    return fpromise::error();
  }
  return fpromise::ok(fidl::ToWire(allocator, result.take_value()));
}

fpromise::result<fuchsia_sysmem::wire::ImageFormat2> ImageConstraintsToFormat(
    const fuchsia_sysmem::wire::ImageFormatConstraints& wire_image_format_constraints_v1,
    uint32_t width, uint32_t height) {
  auto image_format_constraints_v1 = fidl::ToNatural(wire_image_format_constraints_v1);
  auto image_format_constraints_v2_result =
      sysmem::V2CopyFromV1ImageFormatConstraints(image_format_constraints_v1);
  if (!image_format_constraints_v2_result.is_ok()) {
    return fpromise::error();
  }
  auto image_format_constraints_v2 = image_format_constraints_v2_result.take_value();
  auto v2_out_result = ImageConstraintsToFormat(image_format_constraints_v2, width, height);
  if (!v2_out_result.is_ok()) {
    return fpromise::error();
  }
  auto v2_out = v2_out_result.take_value();
  auto v1_out_result = sysmem::V1CopyFromV2ImageFormat(v2_out);
  if (!v1_out_result) {
    return fpromise::error();
  }
  // This arena isn't relied upon by the returned value, because kMaxOutOfLine == 0.
  fidl::Arena arena;
  ZX_DEBUG_ASSERT(fidl::TypeTraits<fuchsia_sysmem::wire::ImageFormat2>::kMaxOutOfLine == 0);
  auto wire_v1 = fidl::ToWire(arena, v1_out_result.take_value());
  return fpromise::ok(wire_v1);
}

bool ImageFormatPlaneByteOffset(const ImageFormat& image_format, uint32_t plane,
                                uint64_t* offset_out) {
  ZX_DEBUG_ASSERT(offset_out);
  auto pixel_format_and_modifier = PixelFormatAndModifierFromImageFormat(image_format);
  for (auto& format_set : kImageFormats) {
    if (format_set->IsSupported(pixel_format_and_modifier)) {
      return format_set->ImageFormatPlaneByteOffset(image_format, plane, offset_out);
    }
  }
  return false;
}

bool ImageFormatPlaneByteOffset(const ImageFormatWire& image_format, uint32_t plane,
                                uint64_t* offset_out) {
  return ImageFormatPlaneByteOffset(fidl::ToNatural(image_format), plane, offset_out);
}

bool ImageFormatPlaneByteOffset(const fuchsia_sysmem::wire::ImageFormat2& wire_image_format_v1,
                                uint32_t plane, uint64_t* offset_out) {
  ZX_DEBUG_ASSERT(offset_out);
  auto image_format_v1 = fidl::ToNatural(wire_image_format_v1);
  auto image_format_v2_result = sysmem::V2CopyFromV1ImageFormat(image_format_v1);
  if (!image_format_v2_result.is_ok()) {
    return false;
  }
  auto image_format_v2 = image_format_v2_result.take_value();
  return ImageFormatPlaneByteOffset(image_format_v2, plane, offset_out);
}

bool ImageFormatPlaneRowBytes(const ImageFormat& image_format, uint32_t plane,
                              uint32_t* row_bytes_out) {
  ZX_DEBUG_ASSERT(row_bytes_out);
  auto pixel_format_and_modifier = PixelFormatAndModifierFromImageFormat(image_format);
  for (auto& format_set : kImageFormats) {
    if (format_set->IsSupported(pixel_format_and_modifier)) {
      return format_set->ImageFormatPlaneRowBytes(image_format, plane, row_bytes_out);
    }
  }
  return false;
}

bool ImageFormatPlaneRowBytes(const ImageFormatWire& wire_image_format, uint32_t plane,
                              uint32_t* row_bytes_out) {
  return ImageFormatPlaneRowBytes(fidl::ToNatural(wire_image_format), plane, row_bytes_out);
}

bool ImageFormatPlaneRowBytes(const fuchsia_sysmem::wire::ImageFormat2& wire_image_format_v1,
                              uint32_t plane, uint32_t* row_bytes_out) {
  ZX_DEBUG_ASSERT(row_bytes_out);
  auto image_format_v1 = fidl::ToNatural(wire_image_format_v1);
  auto image_format_v2_result = sysmem::V2CopyFromV1ImageFormat(image_format_v1);
  if (!image_format_v2_result.is_ok()) {
    return false;
  }
  auto image_format_v2 = image_format_v2_result.take_value();
  return ImageFormatPlaneRowBytes(image_format_v2, plane, row_bytes_out);
}

bool ImageFormatCompatibleWithProtectedMemory(const PixelFormatAndModifier& pixel_format) {
  // AKA kFormatModifierLinear
  if (pixel_format.pixel_format_modifier == fuchsia_images2::kFormatModifierNone) {
    return true;
  }
  constexpr uint64_t kArmLinearFormat = 0x0800000000000000ul;
  switch (pixel_format.pixel_format_modifier & ~AfbcFormats::kAfbcModifierMask) {
    case kArmLinearFormat:
    case fuchsia_images2::kFormatModifierArmAfbc16X16:
    case fuchsia_images2::kFormatModifierArmAfbc32X8:
      // TE formats occasionally need CPU writes to the TE buffer.
      return !(pixel_format.pixel_format_modifier & fuchsia_images2::kFormatModifierArmTeBit);

    default:
      return true;
  }
}

bool ImageFormatCompatibleWithProtectedMemory(
    const fuchsia_sysmem::wire::PixelFormat& wire_pixel_format_v1) {
  auto pixel_format_v1 = fidl::ToNatural(wire_pixel_format_v1);
  auto pixel_format_v2 = sysmem::V2CopyFromV1PixelFormat(pixel_format_v1);
  return ImageFormatCompatibleWithProtectedMemory(pixel_format_v2);
}
