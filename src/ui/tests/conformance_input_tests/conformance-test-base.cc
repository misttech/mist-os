// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/tests/conformance_input_tests/conformance-test-base.h"

#include <fuchsia/ui/test/context/cpp/fidl.h>

namespace ui_conformance_test_base {

void ConformanceTest::SetUp() {
  {
    fuchsia::ui::test::context::RealmFactorySyncPtr realm_factory;
    context_ = sys::ComponentContext::Create();
    ASSERT_EQ(context_->svc()->Connect(realm_factory.NewRequest()), ZX_OK);

    fuchsia::ui::test::context::RealmFactoryCreateRealmRequest req;
    fuchsia::ui::test::context::RealmFactory_CreateRealm_Result res;

    req.set_realm_server(realm_proxy_.NewRequest());
    req.set_display_rotation(DisplayRotation());
    req.set_device_pixel_ratio(DevicePixelRatio());

    ASSERT_EQ(realm_factory->CreateRealm(std::move(req), &res), ZX_OK);
  }
}

const std::shared_ptr<sys::ServiceDirectory>& ConformanceTest::LocalServiceDirectory() const {
  return context_->svc();
}

uint32_t ConformanceTest::DisplayRotation() const { return 0; }

float ConformanceTest::DevicePixelRatio() const { return 1.f; }

}  // namespace ui_conformance_test_base
