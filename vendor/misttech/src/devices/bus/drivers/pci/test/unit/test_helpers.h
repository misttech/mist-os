// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include <lib/inspect/testing/cpp/inspect.h>
#include <lib/zx/vmo.h>

namespace pci_testing {

// This is a port of the zxtest helper to use until the dfv2 test rewrite blows up a lot of tests.
class InspectHelper {
 public:
  void ReadInspect(const zx::vmo& vmo) {
    hierarchy_ = inspect::ReadFromVmo(vmo);
    ASSERT_TRUE(hierarchy_.is_ok());
  }
  inspect::Hierarchy& hierarchy() { return hierarchy_.value(); }

  template <typename T>
  static void CheckProperty(const inspect::NodeValue& node, std::string property,
                            T expected_value) {
    const T* actual_value = node.get_property<T>(property);
    EXPECT_TRUE(actual_value);
    if (!actual_value) {
      return;
    }
    EXPECT_EQ(expected_value.value(), actual_value->value());
  }

 private:
  fpromise::result<inspect::Hierarchy> hierarchy_;
};

}  // namespace pci_testing
