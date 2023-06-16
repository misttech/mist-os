// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/component/cpp/internal/lifecycle.h>
#include <lib/driver/component/cpp/internal/start_args.h>

namespace fdf_internal {

fdf::DriverStartArgs FromEncoded(const EncodedDriverStartArgs& encoded_start_args) {
  // Decode the incoming `msg`.
  auto wire_format_metadata =
      fidl::WireFormatMetadata::FromOpaque(encoded_start_args.wire_format_metadata);
  fdf_internal::AdoptEncodedFidlMessage encoded{encoded_start_args.msg};
  fit::result start_args = fidl::StandaloneDecode<fuchsia_driver_framework::DriverStartArgs>(
      encoded.TakeMessage(), wire_format_metadata);
  ZX_ASSERT_MSG(start_args.is_ok(), "Failed to decode start_args: %s",
                start_args.error_value().FormatDescription().c_str());

  return std::move(start_args.value());
}

}  // namespace fdf_internal
