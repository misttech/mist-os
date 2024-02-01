// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_COBALT_BIN_APP_SYSTEM_DATA_UPDATER_IMPL_H_
#define SRC_COBALT_BIN_APP_SYSTEM_DATA_UPDATER_IMPL_H_

#include <fuchsia/cobalt/cpp/fidl.h>

namespace cobalt {

// TODO: b/315496857 - delete this class once fuchsia::cobalt::SystemDataUpdater is removed and out
// of the support window. This class is no longer used to learn the current channel.
// CurrentChannelProvider is used instead.
class SystemDataUpdaterImpl : public fuchsia::cobalt::SystemDataUpdater {
 public:
  void SetSoftwareDistributionInfo(fuchsia::cobalt::SoftwareDistributionInfo current_info,
                                   SetSoftwareDistributionInfoCallback callback) override;
};

}  // namespace cobalt

#endif  // SRC_COBALT_BIN_APP_SYSTEM_DATA_UPDATER_IMPL_H_
