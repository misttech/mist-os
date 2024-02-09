// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_SCREENSHOT_TESTS_MOCK_IMAGE_COMPRESSION_H_
#define SRC_UI_SCENIC_LIB_SCREENSHOT_TESTS_MOCK_IMAGE_COMPRESSION_H_

#include <fuchsia/ui/compression/internal/cpp/fidl.h>
#include <fuchsia/ui/compression/internal/cpp/fidl_test_base.h>
#include <lib/fidl/cpp/binding_set.h>
#include <lib/syslog/cpp/macros.h>

#include <gmock/gmock.h>

namespace screenshot::test {

// Mock class of BufferCollectionImporter for API testing.
class MockImageCompression
    : public fuchsia::ui::compression::internal::testing::ImageCompressor_TestBase {
 public:
  MockImageCompression() : binding_(this) {}
  void NotImplemented_(const std::string& name) final {}

  void Bind(zx::channel compressor_channel, async_dispatcher_t* dispatcher = nullptr) {
    binding_.Bind(fidl::InterfaceRequest<fuchsia::ui::compression::internal::ImageCompressor>(
                      std::move(compressor_channel)),
                  dispatcher);
  }

  MOCK_METHOD(void, EncodePng,
              (fuchsia::ui::compression::internal::ImageCompressorEncodePngRequest request,
               EncodePngCallback callback));

 private:
  fidl::Binding<fuchsia::ui::compression::internal::ImageCompressor> binding_;
};

}  // namespace screenshot::test

#endif  // SRC_UI_SCENIC_LIB_SCREENSHOT_TESTS_MOCK_IMAGE_COMPRESSION_H_
