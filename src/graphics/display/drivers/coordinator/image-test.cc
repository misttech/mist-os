// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/image.h"

#include <lib/async-loop/loop.h>
#include <lib/async-testing/test_loop.h>
#include <lib/async/cpp/task.h>
#include <lib/async/cpp/wait.h>
#include <lib/async/default.h>
#include <lib/fit/defer.h>

#include <fbl/auto_lock.h>
#include <fbl/ref_ptr.h>
#include <gtest/gtest.h>

#include "src/graphics/display/drivers/coordinator/fence.h"
#include "src/graphics/display/drivers/coordinator/post-display-task.h"
#include "src/graphics/display/drivers/coordinator/testing/base.h"
#include "src/graphics/display/drivers/fake/fake-display.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/image-metadata.h"
#include "src/graphics/display/lib/driver-utils/post-task.h"
#include "src/lib/testing/predicates/status.h"

namespace display_coordinator {

class ImageTest : public TestBase, public FenceCallback {
 public:
  void OnFenceFired(FenceReference* f) override {}
  void OnRefForFenceDead(Fence* fence) override { fence->OnRefDead(); }

  fbl::RefPtr<Image> ImportImage(zx::vmo vmo, const display::ImageMetadata& image_metadata) {
    zx::result<display::DriverImageId> import_result =
        FakeDisplayEngine().ImportVmoImageForTesting(std::move(vmo), /*offset=*/0);
    if (!import_result.is_ok()) {
      return nullptr;
    }

    fbl::RefPtr<Image> image = fbl::AdoptRef(new Image(
        CoordinatorController(), image_metadata, import_result.value(), nullptr, ClientId(1)));
    image->id = next_image_id_++;
    return image;
  }

 private:
  display::ImageId next_image_id_ = display::ImageId(1);
};

}  // namespace display_coordinator
