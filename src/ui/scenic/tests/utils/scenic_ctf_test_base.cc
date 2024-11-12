// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/tests/utils/scenic_ctf_test_base.h"

namespace integration_tests {

void ScenicCtfTest::SetUp() {
  {
    context_ = sys::ComponentContext::Create();
    ASSERT_EQ(context_->svc()->Connect(realm_factory_.NewRequest()), ZX_OK);

    fuchsia::ui::test::context::ScenicRealmFactoryCreateRealmRequest req;
    fuchsia::ui::test::context::ScenicRealmFactory_CreateRealm_Result res;

    req.set_realm_server(realm_proxy_.NewRequest());
    req.set_display_rotation(DisplayRotation());
    req.set_renderer(Renderer());
    req.set_display_composition(DisplayComposition());

    ASSERT_EQ(realm_factory_->CreateRealm(std::move(req), &res), ZX_OK);
  }
}

const std::shared_ptr<sys::ServiceDirectory>& ScenicCtfTest::LocalServiceDirectory() const {
  return context_->svc();
}

uint64_t ScenicCtfTest::DisplayRotation() const { return 0; }

fuchsia::ui::test::context::RendererType ScenicCtfTest::Renderer() const {
  return fuchsia::ui::test::context::RendererType::VULKAN;
}

bool ScenicCtfTest::DisplayComposition() const { return true; }

}  // namespace integration_tests
