// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef EXAMPLES_DRIVERS_METADATA_METADATA_H_
#define EXAMPLES_DRIVERS_METADATA_METADATA_H_

#include <fidl/fuchsia.examples.metadata/cpp/fidl.h>
#include <lib/driver/metadata/cpp/metadata_server.h>

#include "lib/driver/metadata/cpp/metadata.h"

namespace fdf_metadata {

template <>
struct ObjectDetails<fuchsia_examples_metadata::Metadata> {
  inline static const char* Name = fuchsia_examples_metadata::kService;
};

}  // namespace fdf_metadata

namespace fuchsia_examples_metadata {

using MetadataServer = fdf_metadata::MetadataServer<Metadata>;

}  // namespace fuchsia_examples_metadata

#endif  // EXAMPLES_DRIVERS_METADATA_METADATA_H_
