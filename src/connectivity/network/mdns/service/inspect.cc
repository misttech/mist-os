// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "inspect.h"

#include <lib/syslog/cpp/macros.h>

namespace mdns {

MdnsInspector& MdnsInspector::GetMdnsInspector() {
  static MdnsInspector mdns_inspector;
  return mdns_inspector;
}

MdnsInspector::MdnsInspector(async_dispatcher_t* dispatcher)
    : inspector_(
          std::make_unique<inspect::ComponentInspector>(dispatcher, inspect::PublishOptions{})) {}

}  // namespace mdns
