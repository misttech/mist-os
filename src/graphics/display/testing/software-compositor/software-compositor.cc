// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/testing/software-compositor/software-compositor.h"

#include <lib/stdcompat/span.h>
#include <zircon/assert.h>

#include <cstddef>
#include <cstdint>
#include <cstdio>

#include "src/graphics/display/testing/software-compositor/pixel.h"

namespace software_compositor {

namespace {

inline std::pair<cpp20::span<uint8_t>::iterator, cpp20::span<const uint8_t>::iterator> CopyPixel(
    cpp20::span<uint8_t>::iterator canvas_pixel_it, PixelFormat canvas_pixel_format,
    cpp20::span<const uint8_t>::iterator source_pixel_it, PixelFormat source_pixel_format) {
  if (canvas_pixel_format == source_pixel_format) {
    std::tie(canvas_pixel_it[0], canvas_pixel_it[1], canvas_pixel_it[2], canvas_pixel_it[3]) =
        std::tie(source_pixel_it[0], source_pixel_it[1], source_pixel_it[2], source_pixel_it[3]);
  } else if ((canvas_pixel_format == PixelFormat::kBgra8888 &&
              source_pixel_format == PixelFormat::kRgba8888) ||
             (canvas_pixel_format == PixelFormat::kRgba8888 &&
              source_pixel_format == PixelFormat::kBgra8888)) {
    std::tie(canvas_pixel_it[0], canvas_pixel_it[1], canvas_pixel_it[2], canvas_pixel_it[3]) =
        std::tie(source_pixel_it[2], source_pixel_it[1], source_pixel_it[0], source_pixel_it[3]);
  } else {
    // This should never happen.
    ZX_DEBUG_ASSERT_MSG(false, "not implemented");
  }
  return {canvas_pixel_it + GetBytesPerPixel(canvas_pixel_format),
          source_pixel_it + GetBytesPerPixel(source_pixel_format)};
}

}  // namespace

PixelData OutputImage::At(const Offset2D& offset) const {
  cpp20::span<const uint8_t> data{reinterpret_cast<const uint8_t*>(buffer.data()),
                                  static_cast<size_t>(properties.height) * properties.stride_bytes};
  const int bytes_per_pixel = GetBytesPerPixel(properties.pixel_format);
  return PixelData::FromRaw(buffer.subspan(
      offset.y * properties.stride_bytes + offset.x * bytes_per_pixel, bytes_per_pixel));
}

void OutputImage::SetPixelData(const Offset2D& offset, const PixelData& pixel) const {
  const int bytes_per_pixel = GetBytesPerPixel(properties.pixel_format);
  const auto color_to_write = buffer.subspan(
      offset.y * properties.stride_bytes + offset.x * bytes_per_pixel, bytes_per_pixel);
  std::copy(pixel.data.begin(), pixel.data.end(), color_to_write.begin());
}

PixelData InputImage::At(const Offset2D& offset) const {
  const int bytes_per_pixel = GetBytesPerPixel(properties.pixel_format);
  return PixelData::FromRaw(buffer.subspan(
      offset.y * properties.stride_bytes + offset.x * bytes_per_pixel, bytes_per_pixel));
}

SoftwareCompositor::SoftwareCompositor(const OutputImage& canvas) : canvas_(canvas) {}

void SoftwareCompositor::ClearCanvas(const PixelData& color, PixelFormat pixel_format) const {
  const PixelData color_to_fill = color.Convert(pixel_format, canvas_.properties.pixel_format);
  for (int row = 0; row < canvas_.properties.height; ++row) {
    cpp20::span<uint8_t> bytes_to_fill = canvas_.buffer.subspan(
        row * canvas_.properties.stride_bytes, canvas_.properties.stride_bytes);
    auto it = bytes_to_fill.begin();
    for (int column = 0; column < canvas_.properties.width; ++column) {
      it = std::copy(color_to_fill.data.begin(), color_to_fill.data.end(), it);
    }
  }
}

void SoftwareCompositor::CompositeImage(const InputImage& input_image,
                                        const CompositionProperties& composition_properties) const {
  // TODO(fxbug.dev/124683): Currently alpha compositing is not supported.
  // Callers must guarantee that alpha blending is disabled.
  ZX_ASSERT(composition_properties.alpha_mode == ::display::AlphaMode::kDisable);

  // TODO(fxbug.dev/124683): Currently image transformation is not supported.
  // Callers must guarantee that the image to draw is not rotated nor flipped.
  ZX_ASSERT(composition_properties.transform == ::display::Transform::kIdentity);

  const ::display::Frame& canvas_frame = composition_properties.canvas_frame;
  const ::display::Frame& source_frame = composition_properties.source_frame;

  // TODO(fxbug.dev/124683): Currently this doesn't support clipping of the
  // input image. Callers must guarantee that the source frame starts at (0, 0)
  // and has the same size as the input image.
  ZX_ASSERT(source_frame.y_pos == 0);
  ZX_ASSERT(source_frame.x_pos == 0);
  ZX_ASSERT(source_frame.height == input_image.properties.height);
  ZX_ASSERT(source_frame.width == input_image.properties.width);

  // TODO(fxbug.dev/124683): Currently this doesn't support scaling.
  // Callers must guarantee that the destination frame size is the same as the
  // original image size.
  ZX_ASSERT(canvas_frame.height == input_image.properties.height);
  ZX_ASSERT(canvas_frame.width == input_image.properties.width);

  // TODO(fxbug.dev/124683): Currently this doesn't support clipping.
  // Callers must guarantee that the destination frame falls within the canvas.
  ZX_ASSERT(canvas_frame.y_pos + canvas_frame.height <= canvas_.properties.height);
  ZX_ASSERT(canvas_frame.x_pos + canvas_frame.width <= canvas_.properties.width);

  const int src_bytes_per_pixel = GetBytesPerPixel(input_image.properties.pixel_format);
  const int dst_bytes_per_pixel = GetBytesPerPixel(canvas_.properties.pixel_format);

  for (int row = 0; row < canvas_frame.height; row++) {
    cpp20::span<const uint8_t> source_row = input_image.buffer.subspan(
        (row + source_frame.y_pos) * input_image.properties.stride_bytes +
            source_frame.x_pos * src_bytes_per_pixel,
        /*count=*/source_frame.width * src_bytes_per_pixel);
    cpp20::span<uint8_t> canvas_row =
        canvas_.buffer.subspan((row + canvas_frame.y_pos) * canvas_.properties.stride_bytes +
                                   canvas_frame.x_pos * dst_bytes_per_pixel,
                               /*count=*/canvas_frame.width * dst_bytes_per_pixel);

    auto source_row_it = source_row.begin();
    auto canvas_row_it = canvas_row.begin();
    for (int column = 0; column < canvas_frame.width; column++) {
      std::tie(canvas_row_it, source_row_it) =
          CopyPixel(canvas_row_it, canvas_.properties.pixel_format, source_row_it,
                    input_image.properties.pixel_format);
    }
  }
}

void SoftwareCompositor::CompositeImageLayers(
    cpp20::span<const ImageLayerForComposition> image_layers) const {
  constexpr PixelData kBlack = {0x00, 0x00, 0x00, 0xff};
  ClearCanvas(kBlack, PixelFormat::kRgba8888);
  for (const ImageLayerForComposition& image_layer : image_layers) {
    CompositeImage(image_layer.image, image_layer.properties);
  }
}

}  // namespace software_compositor
