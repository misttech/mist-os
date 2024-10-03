// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_CONNECTIVITY_NETWORK_MDNS_SERVICE_INSPECT_H_
#define SRC_CONNECTIVITY_NETWORK_MDNS_SERVICE_INSPECT_H_

#include <lib/inspect/component/cpp/component.h>

#include "lib/async/default.h"

namespace mdns {

namespace testing {
class MdnsInspectorTest;
}  // namespace testing

// This class defines the structure of the Inspect hierarchy for Mdns.

class MdnsInspector final {
 public:
  // Creates/Returns the pointer to MdnsInspector singleton object.
  static MdnsInspector& GetMdnsInspector();
  MdnsInspector(MdnsInspector const&) = delete;
  void operator=(MdnsInspector const&) = delete;

  friend class testing::MdnsInspectorTest;

 private:
  MdnsInspector() : MdnsInspector(async_get_default_dispatcher()) {}
  explicit MdnsInspector(async_dispatcher_t* dispatcher);

  std::unique_ptr<inspect::ComponentInspector> inspector_;
};

}  // namespace mdns
#endif  // SRC_CONNECTIVITY_NETWORK_MDNS_SERVICE_INSPECT_H_
