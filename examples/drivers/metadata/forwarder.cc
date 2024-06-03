// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/component/cpp/driver_export.h>

#include "metadata.h"

class Sender : public fdf::DriverBase {
 public:
  Sender(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase("parent", std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override {
    // Set metadata using the driver's parent driver metadata.
    ZX_ASSERT(metadata_server_.ForwardMetadata(incoming()) == ZX_OK);

    // Serve the metadata to the driver's outgoing directory.
    ZX_ASSERT(metadata_server_.Serve(*outgoing(), dispatcher()) == ZX_OK);

    return zx::ok();
  }

 private:
  // Responsible for serving metadata.
  fdf_metadata::MetadataServer<fuchsia_examples_metadata::Metadata> metadata_server_;
};

FUCHSIA_DRIVER_EXPORT(Sender);
