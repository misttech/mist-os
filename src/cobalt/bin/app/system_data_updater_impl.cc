// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/cobalt/bin/app/system_data_updater_impl.h"

namespace cobalt {

using FuchsiaStatus = fuchsia::cobalt::Status;

void SystemDataUpdaterImpl::SetSoftwareDistributionInfo(
    fuchsia::cobalt::SoftwareDistributionInfo current_info,
    SetSoftwareDistributionInfoCallback callback) {
  // Deprecated in F18 in favor of src/cobalt/bin/app/current_channel_provider.h.
  callback(FuchsiaStatus::OK);
}

}  // namespace cobalt
