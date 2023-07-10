// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fit/function.h>
#include <lib/zx/clock.h>
#include <zircon/assert.h>

#include <chrono>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/graphics/display/testing/software-compositor/pixel.h"
#include "src/graphics/display/testing/software-compositor/software-compositor.h"

namespace software_compositor {
namespace {

TEST(ClearCanvas, NoConversionRgba) {
  constexpr int kCanvasWidth = 1280;
  constexpr int kCanvasHeight = 800;
  std::vector<uint8_t> canvas_bytes(kCanvasHeight * kCanvasWidth * 4, 0);

  OutputImage canvas = {
      .buffer = canvas_bytes,
      .properties =
          {
              .width = kCanvasWidth,
              .height = kCanvasHeight,
              .stride_bytes = kCanvasWidth * 4,
              .pixel_format = PixelFormat::kRgba8888,
          },
  };

  {
    SoftwareCompositor software_compositor(canvas);
    software_compositor.ClearCanvas({1, 2, 3, 255}, PixelFormat::kRgba8888);
  }

  for (int pixel_index = 0; pixel_index < kCanvasHeight * kCanvasWidth; ++pixel_index) {
    const std::vector<uint8_t> pixel_actual(canvas_bytes.begin() + pixel_index * 4,
                                            canvas_bytes.begin() + pixel_index * 4 + 4);
    const std::vector<uint8_t> pixel_expected = {1, 2, 3, 255};
    // EXPECT_THAT(pixel_actual, testing::ElementsAreArray(pixel_expected));
  }
}

TEST(ClearCanvas, NoConversionBgra) {
  constexpr int kCanvasWidth = 1280;
  constexpr int kCanvasHeight = 800;
  std::vector<uint8_t> canvas_bytes(kCanvasHeight * kCanvasWidth * 4, 0);

  OutputImage canvas = {
      .buffer = canvas_bytes,
      .properties =
          {
              .width = kCanvasWidth,
              .height = kCanvasHeight,
              .stride_bytes = kCanvasWidth * 4,
              .pixel_format = PixelFormat::kBgra8888,
          },
  };

  {
    SoftwareCompositor software_compositor(canvas);
    software_compositor.ClearCanvas({1, 2, 3, 255}, PixelFormat::kBgra8888);
  }

  for (size_t bytes_index = 0; bytes_index < canvas_bytes.size(); bytes_index += 4) {
    cpp20::span<uint8_t> pixel_actual(canvas_bytes.begin() + bytes_index, 4);
    std::array<uint8_t, 4> pixel_expected = {1, 2, 3, 255};
    EXPECT_THAT(pixel_actual, testing::ElementsAreArray(pixel_expected));
  }
}

TEST(ClearCanvas, ConversionRgbaToBgra) {
  constexpr int kCanvasWidth = 1280;
  constexpr int kCanvasHeight = 800;
  std::vector<uint8_t> canvas_bytes(kCanvasHeight * kCanvasWidth * 4, 0);

  OutputImage canvas = {
      .buffer = canvas_bytes,
      .properties =
          {
              .width = kCanvasWidth,
              .height = kCanvasHeight,
              .stride_bytes = kCanvasWidth * 4,
              .pixel_format = PixelFormat::kBgra8888,
          },
  };

  {
    SoftwareCompositor software_compositor(canvas);
    software_compositor.ClearCanvas({1, 2, 3, 255}, PixelFormat::kRgba8888);
  }

  for (size_t bytes_index = 0; bytes_index < canvas_bytes.size(); bytes_index += 4) {
    cpp20::span<uint8_t> pixel_actual(canvas_bytes.begin() + bytes_index, 4);
    std::array<uint8_t, 4> pixel_expected = {3, 2, 1, 255};
    EXPECT_THAT(pixel_actual, testing::ElementsAreArray(pixel_expected));
  }
}

TEST(ClearCanvas, ConversionBgraToRgba) {
  constexpr int kCanvasWidth = 1280;
  constexpr int kCanvasHeight = 800;
  std::vector<uint8_t> canvas_bytes(kCanvasHeight * kCanvasWidth * 4, 0);

  OutputImage canvas = {
      .buffer = canvas_bytes,
      .properties =
          {
              .width = kCanvasWidth,
              .height = kCanvasHeight,
              .stride_bytes = kCanvasWidth * 4,
              .pixel_format = PixelFormat::kRgba8888,
          },
  };

  {
    SoftwareCompositor software_compositor(canvas);
    software_compositor.ClearCanvas({1, 2, 3, 255}, PixelFormat::kBgra8888);
  }

  for (size_t bytes_index = 0; bytes_index < canvas_bytes.size(); bytes_index += 4) {
    cpp20::span<uint8_t> pixel_actual(canvas_bytes.begin() + bytes_index, 4);
    std::array<uint8_t, 4> pixel_expected = {3, 2, 1, 255};
    EXPECT_THAT(pixel_actual, testing::ElementsAreArray(pixel_expected));
  }
}

TEST(CompositeImageLayers, NoConversionBgra) {
  constexpr int kCanvasWidth = 1280;
  constexpr int kCanvasHeight = 800;
  std::vector<uint8_t> canvas_bytes(kCanvasHeight * kCanvasWidth * 4, 0);

  std::vector<uint8_t> red_bytes(kCanvasHeight * kCanvasWidth * 4, 0);
  for (size_t i = 0; i < red_bytes.size(); i += 4) {
    red_bytes[i + 2] = red_bytes[i + 3] = 255;
  }

  OutputImage canvas = {
      .buffer = canvas_bytes,
      .properties =
          {
              .width = kCanvasWidth,
              .height = kCanvasHeight,
              .stride_bytes = kCanvasWidth * 4,
              .pixel_format = PixelFormat::kBgra8888,
          },
  };

  InputImage input_image = {
      .buffer = red_bytes,
      .properties =
          {
              .width = kCanvasWidth,
              .height = kCanvasHeight,
              .stride_bytes = kCanvasWidth * 4,
              .pixel_format = PixelFormat::kBgra8888,
          },
  };

  {
    SoftwareCompositor software_compositor(canvas);
    std::vector<software_compositor::SoftwareCompositor::ImageLayerForComposition> image_layers = {
        {
            .image = input_image,
            .properties =
                {
                    .source_frame =
                        {
                            .x_pos = 0,
                            .y_pos = 0,
                            .width = input_image.properties.width,
                            .height = input_image.properties.height,
                        },
                    .canvas_frame =
                        {
                            .x_pos = 0,
                            .y_pos = 0,
                            .width = input_image.properties.width,
                            .height = input_image.properties.height,
                        },
                    .transform = ::display::Transform::kIdentity,
                    .alpha_mode = ::display::AlphaMode::kDisable,
                },
        },
    };
    software_compositor.CompositeImageLayers(image_layers);
  }

  for (size_t bytes_index = 0; bytes_index < canvas_bytes.size(); bytes_index += 4) {
    cpp20::span<uint8_t> pixel_actual(canvas_bytes.begin() + bytes_index, 4);
    std::array<uint8_t, 4> pixel_expected = {0, 0, 255, 255};
    EXPECT_THAT(pixel_actual, testing::ElementsAreArray(pixel_expected));
  }
}

TEST(CompositeImageLayers, NoConversionRgba) {
  constexpr int kCanvasWidth = 1280;
  constexpr int kCanvasHeight = 800;
  std::vector<uint8_t> canvas_bytes(kCanvasHeight * kCanvasWidth * 4, 0);

  std::vector<uint8_t> red_bytes(kCanvasHeight * kCanvasWidth * 4, 0);
  for (size_t i = 0; i < red_bytes.size(); i += 4) {
    red_bytes[i + 0] = red_bytes[i + 3] = 255;
  }

  OutputImage canvas = {
      .buffer = canvas_bytes,
      .properties =
          {
              .width = kCanvasWidth,
              .height = kCanvasHeight,
              .stride_bytes = kCanvasWidth * 4,
              .pixel_format = PixelFormat::kRgba8888,
          },
  };

  InputImage input_image = {
      .buffer = red_bytes,
      .properties =
          {
              .width = kCanvasWidth,
              .height = kCanvasHeight,
              .stride_bytes = kCanvasWidth * 4,
              .pixel_format = PixelFormat::kRgba8888,
          },
  };

  {
    SoftwareCompositor software_compositor(canvas);
    std::vector<software_compositor::SoftwareCompositor::ImageLayerForComposition> image_layers = {
        {
            .image = input_image,
            .properties =
                {
                    .source_frame =
                        {
                            .x_pos = 0,
                            .y_pos = 0,
                            .width = input_image.properties.width,
                            .height = input_image.properties.height,
                        },
                    .canvas_frame =
                        {
                            .x_pos = 0,
                            .y_pos = 0,
                            .width = input_image.properties.width,
                            .height = input_image.properties.height,
                        },
                    .transform = ::display::Transform::kIdentity,
                    .alpha_mode = ::display::AlphaMode::kDisable,
                },
        },
    };
    software_compositor.CompositeImageLayers(image_layers);
  }

  for (size_t bytes_index = 0; bytes_index < canvas_bytes.size(); bytes_index += 4) {
    cpp20::span<uint8_t> pixel_actual(canvas_bytes.begin() + bytes_index, 4);
    std::array<uint8_t, 4> pixel_expected = {255, 0, 0, 255};
    EXPECT_THAT(pixel_actual, testing::ElementsAreArray(pixel_expected));
  }
}

TEST(CompositeImageLayers, ConversionRgbaToBgra) {
  constexpr int kCanvasWidth = 1280;
  constexpr int kCanvasHeight = 800;
  std::vector<uint8_t> canvas_bytes(kCanvasHeight * kCanvasWidth * 4, 0);

  std::vector<uint8_t> blue_bytes(kCanvasHeight * kCanvasWidth * 4, 0);
  for (size_t i = 0; i < blue_bytes.size(); i += 4) {
    blue_bytes[i + 2] = blue_bytes[i + 3] = 255;
  }

  OutputImage canvas = {
      .buffer = canvas_bytes,
      .properties =
          {
              .width = kCanvasWidth,
              .height = kCanvasHeight,
              .stride_bytes = kCanvasWidth * 4,
              .pixel_format = PixelFormat::kBgra8888,
          },
  };

  InputImage input_image = {
      .buffer = blue_bytes,
      .properties =
          {
              .width = kCanvasWidth,
              .height = kCanvasHeight,
              .stride_bytes = kCanvasWidth * 4,
              .pixel_format = PixelFormat::kRgba8888,
          },
  };

  {
    SoftwareCompositor software_compositor(canvas);
    std::vector<software_compositor::SoftwareCompositor::ImageLayerForComposition> image_layers = {
        {
            .image = input_image,
            .properties =
                {
                    .source_frame =
                        {
                            .x_pos = 0,
                            .y_pos = 0,
                            .width = input_image.properties.width,
                            .height = input_image.properties.height,
                        },
                    .canvas_frame =
                        {
                            .x_pos = 0,
                            .y_pos = 0,
                            .width = input_image.properties.width,
                            .height = input_image.properties.height,
                        },
                    .transform = ::display::Transform::kIdentity,
                    .alpha_mode = ::display::AlphaMode::kDisable,
                },
        },
    };
    software_compositor.CompositeImageLayers(image_layers);
  }

  for (size_t bytes_index = 0; bytes_index < canvas_bytes.size(); bytes_index += 4) {
    cpp20::span<uint8_t> pixel_actual(canvas_bytes.begin() + bytes_index, 4);
    std::array<uint8_t, 4> pixel_expected = {255, 0, 0, 255};
    EXPECT_THAT(pixel_actual, testing::ElementsAreArray(pixel_expected));
  }
}

TEST(CompositeImageLayers, SetDestinationFrame) {
  constexpr int kCanvasWidth = 1280;
  constexpr int kCanvasHeight = 800;
  std::vector<uint8_t> canvas_bytes(kCanvasHeight * kCanvasWidth * 4, 0);

  constexpr int kImageWidth = 512;
  constexpr int kImageHeight = 384;
  std::vector<uint8_t> red_bytes(kImageHeight * kImageWidth * 4, 0);
  for (size_t i = 0; i < red_bytes.size(); i += 4) {
    red_bytes[i + 0] = red_bytes[i + 3] = 255;
  }

  OutputImage canvas = {
      .buffer = canvas_bytes,
      .properties =
          {
              .width = kCanvasWidth,
              .height = kCanvasHeight,
              .stride_bytes = kCanvasWidth * 4,
              .pixel_format = PixelFormat::kRgba8888,
          },
  };

  InputImage input_image = {
      .buffer = red_bytes,
      .properties =
          {
              .width = kImageWidth,
              .height = kImageHeight,
              .stride_bytes = kImageWidth * 4,
              .pixel_format = PixelFormat::kRgba8888,
          },
  };

  constexpr int kImageLeft = 100;
  constexpr int kImageTop = 200;

  SoftwareCompositor software_compositor(canvas);
  software_compositor.ClearCanvas({0, 0, 0, 255}, PixelFormat::kRgba8888);

  {
    std::vector<software_compositor::SoftwareCompositor::ImageLayerForComposition> image_layers = {
        {
            .image = input_image,
            .properties =
                {
                    .source_frame =
                        {
                            .x_pos = 0,
                            .y_pos = 0,
                            .width = input_image.properties.width,
                            .height = input_image.properties.height,
                        },
                    .canvas_frame =
                        {
                            .x_pos = kImageLeft,
                            .y_pos = kImageTop,
                            .width = input_image.properties.width,
                            .height = input_image.properties.height,
                        },
                    .transform = ::display::Transform::kIdentity,
                    .alpha_mode = ::display::AlphaMode::kDisable,
                },
        },
    };
    software_compositor.CompositeImageLayers(image_layers);
  }

  for (int row = 0; row < kCanvasHeight; ++row) {
    for (int col = 0; col < kCanvasWidth; ++col) {
      int byte_index = (row * kCanvasWidth + col) * 4;
      std::vector<uint8_t> pixel_actual(canvas_bytes.begin() + byte_index,
                                        canvas_bytes.begin() + byte_index + 4);
      bool in_rectangle = row >= kImageTop && (row < kImageTop + kImageHeight) &&
                          (col >= kImageLeft) && (col < kImageLeft + kImageWidth);
      std::array<uint8_t, 4> pixel_expected = in_rectangle ? std::array<uint8_t, 4>{255, 0, 0, 255}
                                                           : std::array<uint8_t, 4>{0, 0, 0, 255};
      EXPECT_THAT(pixel_actual, testing::ElementsAreArray(pixel_expected))
          << "Pixels differ at row " << row << " column " << col;
    }
  }
}

TEST(CompositeImageLayers, MultipleLayersNoOverlapRgba) {
  constexpr int kCanvasWidth = 1280;
  constexpr int kCanvasHeight = 800;
  std::vector<uint8_t> canvas_bytes(kCanvasHeight * kCanvasWidth * 4, 0);
  OutputImage canvas = {
      .buffer = canvas_bytes,
      .properties =
          {
              .width = kCanvasWidth,
              .height = kCanvasHeight,
              .stride_bytes = kCanvasWidth * 4,
              .pixel_format = PixelFormat::kRgba8888,
          },
  };

  constexpr int kBlueWidth = 640;
  constexpr int kBlueHeight = 800;
  std::vector<uint8_t> blue_bytes_rgba(kCanvasHeight * kCanvasWidth * 4, 0);
  for (size_t i = 0; i < blue_bytes_rgba.size(); i += 4) {
    blue_bytes_rgba[i + 2] = blue_bytes_rgba[i + 3] = 255;
  }
  InputImage blue_image = {
      .buffer = blue_bytes_rgba,
      .properties =
          {
              .width = kBlueWidth,
              .height = kBlueHeight,
              .stride_bytes = kBlueWidth * 4,
              .pixel_format = PixelFormat::kRgba8888,
          },
  };

  constexpr int kRedWidth = 640;
  constexpr int kRedHeight = 800;
  std::vector<uint8_t> red_bytes_rgba(kCanvasHeight * kCanvasWidth * 4, 0);
  for (size_t i = 0; i < red_bytes_rgba.size(); i += 4) {
    red_bytes_rgba[i + 0] = red_bytes_rgba[i + 3] = 255;
  }
  InputImage red_image = {
      .buffer = red_bytes_rgba,
      .properties =
          {
              .width = kRedWidth,
              .height = kRedHeight,
              .stride_bytes = kRedWidth * 4,
              .pixel_format = PixelFormat::kRgba8888,
          },
  };

  {
    SoftwareCompositor software_compositor(canvas);
    std::vector<software_compositor::SoftwareCompositor::ImageLayerForComposition> image_layers = {
        {
            .image = blue_image,
            .properties =
                {
                    .source_frame =
                        {
                            .x_pos = 0,
                            .y_pos = 0,
                            .width = blue_image.properties.width,
                            .height = blue_image.properties.height,
                        },
                    .canvas_frame =
                        {
                            .x_pos = 0,
                            .y_pos = 0,
                            .width = blue_image.properties.width,
                            .height = blue_image.properties.height,
                        },
                    .transform = ::display::Transform::kIdentity,
                    .alpha_mode = ::display::AlphaMode::kDisable,
                },
        },
        {

            .image = red_image,
            .properties =
                {
                    .source_frame =
                        {
                            .x_pos = 0,
                            .y_pos = 0,
                            .width = red_image.properties.width,
                            .height = red_image.properties.height,
                        },
                    .canvas_frame =
                        {
                            .x_pos = 640,
                            .y_pos = 0,
                            .width = red_image.properties.width,
                            .height = red_image.properties.height,
                        },
                    .transform = ::display::Transform::kIdentity,
                    .alpha_mode = ::display::AlphaMode::kDisable,
                },
        },
    };
    software_compositor.CompositeImageLayers(image_layers);
  }

  for (int row = 0; row < kCanvasHeight; ++row) {
    for (int col = 0; col < kCanvasWidth; ++col) {
      int byte_index = (row * kCanvasWidth + col) * 4;
      std::vector<uint8_t> pixel_actual(canvas_bytes.begin() + byte_index,
                                        canvas_bytes.begin() + byte_index + 4);
      bool is_blue = col < 640;
      std::array<uint8_t, 4> pixel_expected =
          is_blue ? std::array<uint8_t, 4>{0, 0, 255, 255} : std::array<uint8_t, 4>{255, 0, 0, 255};
      EXPECT_THAT(pixel_actual, testing::ElementsAreArray(pixel_expected))
          << "Pixels differ at row " << row << " column " << col;
    }
  }
}

TEST(CompositeImageLayers, MultipleLayersOverlapNoAlphaRgba) {
  constexpr int kCanvasWidth = 1280;
  constexpr int kCanvasHeight = 800;
  std::vector<uint8_t> canvas_bytes(kCanvasHeight * kCanvasWidth * 4, 0);
  for (size_t i = 0; i < canvas_bytes.size(); i += 4) {
    canvas_bytes[i + 3] = 255;
  }
  OutputImage canvas = {
      .buffer = canvas_bytes,
      .properties =
          {
              .width = kCanvasWidth,
              .height = kCanvasHeight,
              .stride_bytes = kCanvasWidth * 4,
              .pixel_format = PixelFormat::kRgba8888,
          },
  };

  constexpr int kBlueWidth = 640;
  constexpr int kBlueHeight = 800;
  std::vector<uint8_t> blue_bytes_rgba(kCanvasHeight * kCanvasWidth * 4, 0);
  for (size_t i = 0; i < blue_bytes_rgba.size(); i += 4) {
    blue_bytes_rgba[i + 2] = blue_bytes_rgba[i + 3] = 255;
  }
  InputImage blue_image = {
      .buffer = blue_bytes_rgba,
      .properties =
          {
              .width = kBlueWidth,
              .height = kBlueHeight,
              .stride_bytes = kBlueWidth * 4,
              .pixel_format = PixelFormat::kRgba8888,
          },
  };

  constexpr int kRedWidth = 640;
  constexpr int kRedHeight = 800;
  std::vector<uint8_t> red_bytes_rgba(kCanvasHeight * kCanvasWidth * 4, 0);
  for (size_t i = 0; i < red_bytes_rgba.size(); i += 4) {
    red_bytes_rgba[i + 0] = red_bytes_rgba[i + 3] = 255;
  }
  InputImage red_image = {
      .buffer = red_bytes_rgba,
      .properties =
          {
              .width = kRedWidth,
              .height = kRedHeight,
              .stride_bytes = kRedWidth * 4,
              .pixel_format = PixelFormat::kRgba8888,
          },
  };

  {
    SoftwareCompositor software_compositor(canvas);
    std::vector<software_compositor::SoftwareCompositor::ImageLayerForComposition> image_layers = {
        {
            .image = blue_image,
            .properties =
                {
                    .source_frame =
                        {
                            .x_pos = 0,
                            .y_pos = 0,
                            .width = blue_image.properties.width,
                            .height = blue_image.properties.height,
                        },
                    .canvas_frame =
                        {
                            .x_pos = 0,
                            .y_pos = 0,
                            .width = blue_image.properties.width,
                            .height = blue_image.properties.height,
                        },
                    .transform = ::display::Transform::kIdentity,
                    .alpha_mode = ::display::AlphaMode::kDisable,
                },
        },
        {
            .image = red_image,
            .properties =
                {
                    .source_frame =
                        {
                            .x_pos = 0,
                            .y_pos = 0,
                            .width = red_image.properties.width,
                            .height = red_image.properties.height,
                        },
                    .canvas_frame =
                        {
                            .x_pos = 320,
                            .y_pos = 0,
                            .width = red_image.properties.width,
                            .height = red_image.properties.height,
                        },
                    .transform = ::display::Transform::kIdentity,
                    .alpha_mode = ::display::AlphaMode::kDisable,
                },
        },
    };
    software_compositor.CompositeImageLayers(image_layers);
  }

  for (int row = 0; row < kCanvasHeight; ++row) {
    for (int col = 0; col < kCanvasWidth; ++col) {
      int byte_index = (row * kCanvasWidth + col) * 4;
      std::vector<uint8_t> pixel_actual(canvas_bytes.begin() + byte_index,
                                        canvas_bytes.begin() + byte_index + 4);
      bool is_blue = col < 320;
      bool is_red = col >= 320 && col < 960;
      std::array<uint8_t, 4> pixel_expected = is_blue  ? std::array<uint8_t, 4>{0, 0, 255, 255}
                                              : is_red ? std::array<uint8_t, 4>{255, 0, 0, 255}
                                                       : std::array<uint8_t, 4>{0, 0, 0, 255};
      EXPECT_THAT(pixel_actual, testing::ElementsAreArray(pixel_expected))
          << "Pixels differ at row " << row << " column " << col;
    }
  }
}

TEST(CompositeImageLayers, MultipleLayersNoAlphaMixedRgbaAndBgra) {
  constexpr int kCanvasWidth = 1280;
  constexpr int kCanvasHeight = 800;
  std::vector<uint8_t> canvas_bytes(kCanvasHeight * kCanvasWidth * 4, 0);
  for (size_t i = 0; i < canvas_bytes.size(); i += 4) {
    canvas_bytes[i + 3] = 255;
  }
  OutputImage canvas = {
      .buffer = canvas_bytes,
      .properties =
          {
              .width = kCanvasWidth,
              .height = kCanvasHeight,
              .stride_bytes = kCanvasWidth * 4,
              .pixel_format = PixelFormat::kRgba8888,
          },
  };

  constexpr int kBlueWidth = 640;
  constexpr int kBlueHeight = 800;
  std::vector<uint8_t> blue_bytes_bgra(kCanvasHeight * kCanvasWidth * 4, 0);
  for (size_t i = 0; i < blue_bytes_bgra.size(); i += 4) {
    blue_bytes_bgra[i + 0] = blue_bytes_bgra[i + 3] = 255;
  }
  InputImage blue_image = {
      .buffer = blue_bytes_bgra,
      .properties =
          {
              .width = kBlueWidth,
              .height = kBlueHeight,
              .stride_bytes = kBlueWidth * 4,
              .pixel_format = PixelFormat::kBgra8888,
          },
  };

  constexpr int kRedWidth = 640;
  constexpr int kRedHeight = 800;
  std::vector<uint8_t> red_bytes_rgba(kCanvasHeight * kCanvasWidth * 4, 0);
  for (size_t i = 0; i < red_bytes_rgba.size(); i += 4) {
    red_bytes_rgba[i + 0] = red_bytes_rgba[i + 3] = 255;
  }
  InputImage red_image = {
      .buffer = red_bytes_rgba,
      .properties =
          {
              .width = kRedWidth,
              .height = kRedHeight,
              .stride_bytes = kRedWidth * 4,
              .pixel_format = PixelFormat::kRgba8888,
          },
  };

  {
    SoftwareCompositor software_compositor(canvas);
    std::vector<software_compositor::SoftwareCompositor::ImageLayerForComposition> image_layers = {
        {
            .image = blue_image,
            .properties =
                {
                    .source_frame =
                        {
                            .x_pos = 0,
                            .y_pos = 0,
                            .width = blue_image.properties.width,
                            .height = blue_image.properties.height,
                        },
                    .canvas_frame =
                        {
                            .x_pos = 0,
                            .y_pos = 0,
                            .width = blue_image.properties.width,
                            .height = blue_image.properties.height,
                        },
                    .transform = ::display::Transform::kIdentity,
                    .alpha_mode = ::display::AlphaMode::kDisable,
                },
        },
        {
            .image = red_image,
            .properties =
                {
                    .source_frame =
                        {
                            .x_pos = 0,
                            .y_pos = 0,
                            .width = red_image.properties.width,
                            .height = red_image.properties.height,
                        },
                    .canvas_frame =
                        {
                            .x_pos = 640,
                            .y_pos = 0,
                            .width = red_image.properties.width,
                            .height = red_image.properties.height,
                        },
                    .transform = ::display::Transform::kIdentity,
                    .alpha_mode = ::display::AlphaMode::kDisable,
                },
        },
    };
    software_compositor.CompositeImageLayers(image_layers);
  }

  for (int row = 0; row < kCanvasHeight; ++row) {
    for (int col = 0; col < kCanvasWidth; ++col) {
      int byte_index = (row * kCanvasWidth + col) * 4;
      std::vector<uint8_t> pixel_actual(canvas_bytes.begin() + byte_index,
                                        canvas_bytes.begin() + byte_index + 4);
      bool is_blue = col < 640;
      std::array<uint8_t, 4> pixel_expected =
          is_blue ? std::array<uint8_t, 4>{0, 0, 255, 255} : std::array<uint8_t, 4>{255, 0, 0, 255};
      EXPECT_THAT(pixel_actual, testing::ElementsAreArray(pixel_expected))
          << "Pixels differ at row " << row << " column " << col;
    }
  }
}

}  // namespace

}  // namespace software_compositor
