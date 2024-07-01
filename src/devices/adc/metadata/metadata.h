// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.adcimpl/cpp/driver/fidl.h>

#include <sdk/lib/driver/metadata/cpp/metadata_server.h>

#ifndef SRC_DEVICES_ADC_METADATA_METADATA_H_
#define SRC_DEVICES_ADC_METADATA_METADATA_H_

namespace fdf_metadata {

template <>
struct ObjectDetails<fuchsia_hardware_adcimpl::Metadata> {
  inline static const char* Name = fuchsia_hardware_adcimpl::kMetadataService;
};

}  // namespace fdf_metadata

namespace adc {

using MetadataServer = fdf_metadata::MetadataServer<fuchsia_hardware_adcimpl::Metadata>;

}  // namespace adc

#endif  // SRC_DEVICES_ADC_METADATA_METADATA_H_
