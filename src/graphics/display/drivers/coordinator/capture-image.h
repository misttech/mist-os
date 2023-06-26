// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_CAPTURE_IMAGE_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_CAPTURE_IMAGE_H_

#include <lib/inspect/cpp/inspect.h>

#include <cstdint>

#include <fbl/ref_counted.h>

#include "src/graphics/display/drivers/coordinator/id-map.h"

namespace display {

class Controller;

class CaptureImage : public fbl::RefCounted<CaptureImage>,
                     public IdMappable<fbl::RefPtr<CaptureImage>, /*IdType=*/uint64_t> {
 public:
  CaptureImage(Controller* controller, uint64_t capture_image_handle, inspect::Node* parent_node,
               uint32_t client_id);
  ~CaptureImage();

  uint64_t capture_image_handle() const { return capture_image_handle_; }
  uint32_t client_id() const { return client_id_; }

 private:
  void InitializeInspect(inspect::Node* parent_node);

  uint64_t capture_image_handle_;
  uint32_t client_id_;

  Controller* const controller_;

  inspect::Node node_;
  inspect::ValueList properties_;
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_CAPTURE_IMAGE_H_
