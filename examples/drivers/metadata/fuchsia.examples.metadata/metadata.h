// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef EXAMPLES_DRIVERS_METADATA_FUCHSIA_EXAMPLES_METADATA_METADATA_H_
#define EXAMPLES_DRIVERS_METADATA_FUCHSIA_EXAMPLES_METADATA_METADATA_H_

#include <fidl/fuchsia.examples.metadata/cpp/fidl.h>
#include <lib/driver/metadata/cpp/metadata.h>
#include <lib/driver/metadata/cpp/metadata_server.h>

namespace fdf_metadata {

// The class specialization of `fdf_metadata::ObjectDetails` for the type
// `fuchsia_examples_metadata::Metadata` must be defined in order for the
// //sdk/lib/driver/metadata library to send and retrieve
// `fuchsia_examples_metadata::Metadata` metadata.
template <>
struct ObjectDetails<fuchsia_examples_metadata::Metadata> {
  // Drivers that want to send `fuchsia_examples_metadata::Metadata` metadata
  // should expose the
  // `ObjectDetails<fuchsia_examples_metadata::Metadata>::Name` service in
  // their component manifest. Drivers that want to retrieve
  // `fuchsia_examples_metadata::Metadata` metadata should declare their usage
  // of the `ObjectDetails<fuchsia_examples_metadata::Metadata>::Name` service
  // in their component manifest.
  inline static const char* Name = fuchsia_examples_metadata::kMetadataService;
};

}  // namespace fdf_metadata

namespace examples::drivers::metadata {

// A metadata server that serves `fuchsia_examples_metadata::Metadata` metadata.
using MetadataServer = fdf_metadata::MetadataServer<fuchsia_examples_metadata::Metadata>;

}  // namespace examples::drivers::metadata

#endif  // EXAMPLES_DRIVERS_METADATA_FUCHSIA_EXAMPLES_METADATA_METADATA_H_
