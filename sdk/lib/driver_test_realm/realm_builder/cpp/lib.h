// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_TEST_REALM_REALM_BUILDER_CPP_LIB_H_
#define LIB_DRIVER_TEST_REALM_REALM_BUILDER_CPP_LIB_H_

#include <fidl/fuchsia.component.test/cpp/fidl.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>

namespace driver_test_realm {

// Sets up the DriverTestRealm component in the given `realm_builder`.
void Setup(component_testing::RealmBuilder& realm_builder);

void AddDtrExposes(component_testing::RealmBuilder& realm_builder,
                   const std::vector<fuchsia_component_test::Capability>& exposes);

void AddDtrOffers(component_testing::RealmBuilder& realm_builder, component_testing::Ref from,
                  const std::vector<fuchsia_component_test::Capability>& offers);

}  // namespace driver_test_realm

#endif  // LIB_DRIVER_TEST_REALM_REALM_BUILDER_CPP_LIB_H_
