// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_POWER_CPP_ELEMENT_DESCRIPTION_H
#define LIB_DRIVER_POWER_CPP_ELEMENT_DESCRIPTION_H

#include <fidl/fuchsia.hardware.power/cpp/fidl.h>

#include "sdk/lib/driver/power/cpp/power-support.h"

namespace fdf_power {

class ElementDesc {
 public:
  fuchsia_hardware_power::wire::PowerElementConfiguration element_config_;
  TokenMap tokens_;
  zx::event active_token_;
  zx::event passive_token_;
  std::pair<fidl::ServerEnd<fuchsia_power_broker::CurrentLevel>,
            fidl::ServerEnd<fuchsia_power_broker::RequiredLevel>>
      level_control_servers_;
  fidl::ServerEnd<fuchsia_power_broker::Lessor> lessor_server_;

  // The below are created if the caller did not supply their corresponding server end
  std::optional<fidl::ClientEnd<fuchsia_power_broker::CurrentLevel>> current_level_client_;
  std::optional<fidl::ClientEnd<fuchsia_power_broker::RequiredLevel>> required_level_client_;
  std::optional<fidl::ClientEnd<fuchsia_power_broker::Lessor>> lessor_client_;
};

}  // namespace fdf_power

#endif /* LIB_DRIVER_POWER_CPP_ELEMENT_DESCRIPTION_H */
