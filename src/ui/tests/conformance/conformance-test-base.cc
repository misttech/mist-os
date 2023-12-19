// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/tests/conformance/conformance-test-base.h"

#include <fuchsia/ui/test/context/cpp/fidl.h>
#include <lib/sys/cpp/component_context.h>

#include "lib/zx/channel.h"

namespace ui_conformance_test_base {

void ConformanceTest::SetUp() {
  {
    fuchsia::ui::test::context::RealmFactorySyncPtr realm_factory;
    auto context = sys::ComponentContext::Create();
    ASSERT_EQ(context->svc()->Connect(realm_factory.NewRequest()), ZX_OK);

    fuchsia::ui::test::context::RealmFactoryCreateRealmRequest req;
    fuchsia::ui::test::context::RealmFactory_CreateRealm_Result res;

    req.set_realm_server(realm_proxy_.NewRequest());

    ASSERT_EQ(realm_factory->CreateRealm(std::move(req), &res), ZX_OK);
  }
}

}  // namespace ui_conformance_test_base
