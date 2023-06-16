// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_CAPTURE_IMAGE_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_CAPTURE_IMAGE_H_

#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/inspect/cpp/inspect.h>

#include <cstdint>

#include <fbl/ref_counted.h>

#include "src/graphics/display/drivers/coordinator/id-map.h"

namespace display {

class Controller;

class CaptureImage : public fbl::RefCounted<CaptureImage>,
                     public IdMappable<fbl::RefPtr<CaptureImage>, /*IdType=*/uint64_t> {
 public:
  CaptureImage(Controller* controller, const image_t& info, inspect::Node* parent_node,
               uint32_t client_id);
  ~CaptureImage();

  image_t& info() { return info_; }
  uint32_t client_id() const { return client_id_; }

 private:
  void InitializeInspect(inspect::Node* parent_node);

  image_t info_;

  Controller* const controller_;

  uint32_t client_id_;

  inspect::Node node_;
  inspect::ValueList properties_;
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_CAPTURE_IMAGE_H_
