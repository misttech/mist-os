// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/driver-framework-migration-utils/metadata/metadata-getter-dfv2.h"

#include <fidl/fuchsia.component.runner/cpp/fidl.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/incoming/cpp/constants.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/internal/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>

#include <gtest/gtest.h>

#include "src/lib/testing/predicates/status.h"

namespace display {

namespace {

struct TestMetadata {
  uint32_t vendor_id;
  uint32_t platform_id;
  uint32_t device_id;
};

constexpr uint32_t kTestMetadataType = 'TEST';

constexpr TestMetadata kTestMetadata = {
    .vendor_id = 0x1a'2b'3c'4d,
    .platform_id = 0x5a'6b'7c'8d,
    .device_id = 0x9a'ab'bc'cd,
};

struct IncomingNamespace {
  fdf_testing::TestNode node{std::string("root")};
  fdf_testing::internal::TestEnvironment env{fdf::Dispatcher::GetCurrent()->get()};
  compat::DeviceServer compat_server;
};

// WARNING: Don't use this test as a template for new tests as it uses the old driver testing
// library.
class MetadataGetterDfv2Test : public testing::Test {
 public:
  // implements `testing::Test`.
  void SetUp() override {
    zx::result<fdf_testing::TestNode::CreateStartArgsResult> start_args_result = incoming_.SyncCall(
        [&](IncomingNamespace* incoming) { return incoming->node.CreateStartArgsAndServe(); });

    ASSERT_TRUE(start_args_result.is_ok());

    zx::result test_environment_init_result = incoming_.SyncCall([&](IncomingNamespace* incoming) {
      return incoming->env.Initialize(std::move(start_args_result->incoming_directory_server));
    });
    ASSERT_TRUE(test_environment_init_result.is_ok());

    incoming_.SyncCall([&](IncomingNamespace* incoming) {
      incoming->compat_server.Initialize(component::kDefaultInstance);

      zx_status_t add_metadata_status = incoming->compat_server.AddMetadata(
          kTestMetadataType, &kTestMetadata, sizeof(TestMetadata));
      ASSERT_OK(add_metadata_status);

      zx_status_t serve_status = incoming->compat_server.Serve(env_dispatcher_->async_dispatcher(),
                                                               &incoming->env.incoming_directory());
      ASSERT_OK(serve_status);
    });

    zx::result<fdf::Namespace> namespace_result =
        fdf::Namespace::Create(*start_args_result->start_args.incoming());
    ASSERT_OK(namespace_result.status_value());

    namespace_ = std::make_shared<fdf::Namespace>(std::move(namespace_result).value());
  }

 protected:
  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher env_dispatcher_ = runtime_.StartBackgroundDispatcher();

  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{
      env_dispatcher_->async_dispatcher(), std::in_place};

  std::shared_ptr<fdf::Namespace> namespace_;
};

TEST_F(MetadataGetterDfv2Test, GetMetadata) {
  zx::result<std::unique_ptr<MetadataGetter>> create_metadata_getter_result =
      display::MetadataGetterDfv2::Create(namespace_);
  ASSERT_OK(create_metadata_getter_result.status_value());

  std::unique_ptr<MetadataGetter> metadata_getter =
      std::move(create_metadata_getter_result).value();

  zx::result<std::unique_ptr<TestMetadata>> metadata_result =
      metadata_getter->Get<TestMetadata>(kTestMetadataType, component::kDefaultInstance);
  ASSERT_OK(metadata_result.status_value());

  std::unique_ptr<TestMetadata> metadata = std::move(metadata_result).value();
  ASSERT_NE(metadata, nullptr);

  EXPECT_EQ(metadata->vendor_id, kTestMetadata.vendor_id);
  EXPECT_EQ(metadata->platform_id, kTestMetadata.platform_id);
  EXPECT_EQ(metadata->device_id, kTestMetadata.device_id);
}

TEST_F(MetadataGetterDfv2Test, ErrorOnIncorrectType) {
  zx::result<std::unique_ptr<MetadataGetter>> create_metadata_getter_result =
      display::MetadataGetterDfv2::Create(namespace_);
  ASSERT_OK(create_metadata_getter_result.status_value());

  std::unique_ptr<MetadataGetter> metadata_getter =
      std::move(create_metadata_getter_result).value();

  static constexpr uint32_t kInvalidMetadataType = 'INVA';
  zx::result<std::unique_ptr<TestMetadata>> metadata_result =
      metadata_getter->Get<TestMetadata>(kInvalidMetadataType, component::kDefaultInstance);
  EXPECT_NE(metadata_result.status_value(), ZX_OK);
}

TEST_F(MetadataGetterDfv2Test, ErrorOnIncorrectReturnType) {
  zx::result<std::unique_ptr<MetadataGetter>> create_metadata_getter_result =
      display::MetadataGetterDfv2::Create(namespace_);
  ASSERT_OK(create_metadata_getter_result.status_value());

  std::unique_ptr<MetadataGetter> metadata_getter =
      std::move(create_metadata_getter_result).value();

  struct DifferentFromTestMetadata {
    uint32_t vendor_id;
    uint32_t platform_id;
    uint32_t device_id;
    uint32_t serial_number;
  };
  zx::result<std::unique_ptr<DifferentFromTestMetadata>> metadata_result =
      metadata_getter->Get<DifferentFromTestMetadata>(kTestMetadataType,
                                                      component::kDefaultInstance);
  EXPECT_NE(metadata_result.status_value(), ZX_OK);
}

TEST_F(MetadataGetterDfv2Test, ErrorOnIncorrectInstance) {
  zx::result<std::unique_ptr<MetadataGetter>> create_metadata_getter_result =
      display::MetadataGetterDfv2::Create(namespace_);
  ASSERT_OK(create_metadata_getter_result.status_value());

  std::unique_ptr<MetadataGetter> metadata_getter =
      std::move(create_metadata_getter_result).value();

  zx::result<std::unique_ptr<TestMetadata>> metadata_result =
      metadata_getter->Get<TestMetadata>(kTestMetadataType, "incorrect-instance");
  EXPECT_NE(metadata_result.status_value(), ZX_OK);
}

}  // namespace

}  // namespace display
