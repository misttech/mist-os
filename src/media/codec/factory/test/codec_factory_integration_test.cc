// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/gpu/magma/cpp/fidl_test_base.h>
#include <fuchsia/media/cpp/fidl.h>
#include <fuchsia/mediacodec/cpp/fidl.h>
#include <fuchsia/sysinfo/cpp/fidl_test_base.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fit/defer.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>
#include <lib/vfs/cpp/pseudo_dir.h>
#include <lib/vfs/cpp/remote_dir.h>

#include <gtest/gtest.h>

#include "sdk/lib/fidl/cpp/binding_set.h"
#include "src/lib/storage/vfs/cpp/pseudo_dir.h"
#include "src/lib/storage/vfs/cpp/service.h"
#include "src/lib/storage/vfs/cpp/synchronous_vfs.h"
#include "src/lib/testing/loop_fixture/real_loop_fixture.h"

// NOLINTNEXTLINE
using namespace component_testing;

class FakeSysInfoDevice : public fuchsia::sysinfo::testing::SysInfo_TestBase {
 public:
  void NotImplemented_(const std::string& name) override {
    fprintf(stderr, "FakeSysInfoDevice doing notimplemented with %s\n", name.c_str());
  }
  void GetBoardName(GetBoardNameCallback callback) override { callback(ZX_OK, "FakeBoard"); }
  fidl::InterfaceRequestHandler<fuchsia::sysinfo::SysInfo> GetHandler() {
    return bindings_.GetHandler(this);
  }

 private:
  fidl::BindingSet<fuchsia::sysinfo::SysInfo> bindings_;
};

class MockSysInfoComponent : public LocalComponent {
 public:
  void Start(std::unique_ptr<LocalComponentHandles> mock_handles) override {
    handles_ = std::move(mock_handles);
    handles_->outgoing()->AddPublicService(sysinfo_device_.GetHandler());
  }

 private:
  FakeSysInfoDevice sysinfo_device_;
  std::unique_ptr<LocalComponentHandles> handles_;
};

class FakeMagmaDevice : public fuchsia::gpu::magma::testing::CombinedDevice_TestBase {
 public:
  void NotImplemented_(const std::string& name) override {
    fprintf(stderr, "Magma doing notimplemented with %s\n", name.c_str());
  }

  void GetIcdList(GetIcdListCallback callback) override {
    std::vector<fuchsia::gpu::magma::IcdInfo> vec;
    if (has_icds_) {
      fuchsia::gpu::magma::IcdInfo info;
      info.set_component_url("#meta/fake_codec_factory.cm");
      info.set_flags(fuchsia::gpu::magma::IcdFlags::SUPPORTS_MEDIA_CODEC_FACTORY);
      vec.push_back(std::move(info));
    }
    callback(std::move(vec));
  }

  fidl::InterfaceRequestHandler<fuchsia::gpu::magma::CombinedDevice> GetHandler() {
    return bindings_.GetHandler(this);
  }

  void CloseAll() { bindings_.CloseAll(); }

  void set_has_icds(bool has_icds) { has_icds_ = has_icds; }

 private:
  fidl::BindingSet<fuchsia::gpu::magma::CombinedDevice> bindings_;
  bool has_icds_ = true;
};

class MockGpuComponent : public LocalComponent {
 public:
  explicit MockGpuComponent(async_dispatcher_t* dispatcher, FakeMagmaDevice& magma_device)
      : magma_device_(magma_device), gpu_vfs_(dispatcher), mediacodec_vfs_(dispatcher) {}

  void Start(std::unique_ptr<LocalComponentHandles> mock_handles) override {
    mock_handles_ = std::move(mock_handles);
    // Use fs:: versions because they support device watcher.
    {
      fidl::InterfaceHandle<fuchsia::io::Directory> io_dir;
      auto gpu_root = fbl::MakeRefCounted<fs::PseudoDir>();
      EXPECT_EQ(ZX_OK, gpu_vfs_.ServeDirectory(gpu_root, fidl::ServerEnd<fuchsia_io::Directory>(
                                                             io_dir.NewRequest().TakeChannel())));
      gpu_root->AddEntry(
          "000", fbl::MakeRefCounted<fs::Service>([this](zx::channel channel) {
            magma_device_.GetHandler()(
                fidl::InterfaceRequest<fuchsia::gpu::magma::CombinedDevice>(std::move(channel)));
            return ZX_OK;
          }));

      EXPECT_EQ(ZX_OK, mock_handles_->outgoing()->root_dir()->AddEntry(
                           "dev-gpu", std::make_unique<vfs::RemoteDir>(io_dir.TakeChannel())));
    }

    {
      fidl::InterfaceHandle<fuchsia::io::Directory> io_dir;
      auto gpu_root = fbl::MakeRefCounted<fs::PseudoDir>();
      EXPECT_EQ(ZX_OK,
                mediacodec_vfs_.ServeDirectory(gpu_root, fidl::ServerEnd<fuchsia_io::Directory>(
                                                             io_dir.NewRequest().TakeChannel())));

      EXPECT_EQ(ZX_OK,
                mock_handles_->outgoing()->root_dir()->AddEntry(
                    "dev-mediacodec", std::make_unique<vfs::RemoteDir>(io_dir.TakeChannel())));
    }
  }

 private:
  FakeMagmaDevice& magma_device_;
  std::unique_ptr<LocalComponentHandles> mock_handles_;
  fs::SynchronousVfs gpu_vfs_;
  fs::SynchronousVfs mediacodec_vfs_;
};

constexpr auto kCodecFactoryName = "codec_factory";
constexpr auto kMockGpuName = "mock_gpu";
constexpr auto kSysInfoName = "mock_sys_info";

class Integration : public gtest::RealLoopFixture {
 protected:
  Integration() = default;

  void InitializeRoutes(RealmBuilder& builder) {
    builder.AddChild(kCodecFactoryName, "#meta/codec_factory.cm");
    builder.AddRoute(Route{
        .capabilities = {Protocol{"fuchsia.logger.LogSink"}},
        .source = ParentRef(),
        .targets = {ChildRef{kCodecFactoryName}},
    });
    builder.AddRoute(Route{
        .capabilities = {Protocol{"fuchsia.mediacodec.CodecFactory"}},
        .source = ChildRef{kCodecFactoryName},
        .targets = {ParentRef()},
    });
    builder.AddLocalChild(kMockGpuName, &mock_gpu_);
    builder.AddLocalChild(kSysInfoName, &mock_sys_info_);
    builder.AddRoute(Route{
        .capabilities = {Protocol{"fuchsia.sysinfo.SysInfo"}},
        .source = ChildRef{kSysInfoName},
        .targets = {ChildRef{kCodecFactoryName}},
    });

    builder.AddRoute(Route{
        .capabilities =
            {
                Directory{
                    .name = "dev-gpu",
                    .rights = fuchsia::io::R_STAR_DIR,
                    .path = "/dev-gpu",
                },
                Directory{
                    .name = "dev-mediacodec",
                    .rights = fuchsia::io::R_STAR_DIR,
                    .path = "/dev-mediacodec",
                },
            },
        .source = ChildRef{kMockGpuName},
        .targets = {ChildRef{kCodecFactoryName}},
    });
  }

  FakeMagmaDevice magma_device_;
  MockGpuComponent mock_gpu_{dispatcher(), magma_device_};
  MockSysInfoComponent mock_sys_info_;
};

TEST_F(Integration, MagmaDevice) {
  auto builder = RealmBuilder::Create();
  InitializeRoutes(builder);
  auto realm = builder.Build(dispatcher());
  auto cleanup = fit::defer([&]() {
    bool complete = false;
    realm.Teardown([&](fit::result<fuchsia::component::Error> result) { complete = true; });
    RunLoopUntil([&]() { return complete; });
  });
  auto factory = realm.component().Connect<fuchsia::mediacodec::CodecFactory>();

  factory.set_error_handler([&](zx_status_t status) { FAIL() << zx_status_get_string(status); });

  fuchsia::mediacodec::CreateDecoder_Params params;
  fuchsia::media::FormatDetails input_details;
  input_details.set_mime_type("video/h264");
  params.set_input_details(std::move(input_details));
  params.set_require_hw(true);
  fuchsia::media::StreamProcessorPtr processor;
  factory->CreateDecoder(std::move(params), processor.NewRequest());
  processor.set_error_handler([&](zx_status_t status) { FAIL() << zx_status_get_string(status); });

  bool on_input_constraints_called = false;
  processor.events().OnInputConstraints = [&](fuchsia::media::StreamBufferConstraints constraints) {
    on_input_constraints_called = true;
    processor.Unbind();
  };

  RunLoopUntil([&]() { return on_input_constraints_called || HasFailure(); });

  magma_device_.CloseAll();

  // Eventually codecs from the device should disappear.
  while (true) {
    fuchsia::mediacodec::CreateDecoder_Params params;
    fuchsia::media::FormatDetails input_details;
    input_details.set_mime_type("video/h264");
    params.set_input_details(std::move(input_details));
    params.set_require_hw(true);
    fuchsia::media::StreamProcessorPtr processor;
    factory->CreateDecoder(std::move(params), processor.NewRequest());

    bool processor_failed = false;
    processor.set_error_handler([&](zx_status_t status) { processor_failed = true; });

    bool on_input_constraints_called = false;
    processor.events().OnInputConstraints =
        [&](fuchsia::media::StreamBufferConstraints constraints) {
          on_input_constraints_called = true;
          processor.Unbind();
        };
    RunLoopUntil([&]() { return processor_failed || on_input_constraints_called; });
    if (processor_failed) {
      break;
    }
    // Ignore this success and try again.
  }
}

// If the Magma Device doesn't list any ICDs, creating a hardware codec should fail but not hang.
TEST_F(Integration, MagmaDeviceNoIcd) {
  auto builder = RealmBuilder::Create();
  InitializeRoutes(builder);
  magma_device_.set_has_icds(false);

  auto realm = builder.Build(dispatcher());
  auto cleanup = fit::defer([&]() {
    bool complete = false;
    realm.Teardown([&](fit::result<fuchsia::component::Error> result) { complete = true; });
    RunLoopUntil([&]() { return complete; });
  });
  auto factory = realm.component().Connect<fuchsia::mediacodec::CodecFactory>();

  factory.set_error_handler([&](zx_status_t status) { FAIL() << zx_status_get_string(status); });

  fuchsia::mediacodec::CreateDecoder_Params params;
  fuchsia::media::FormatDetails input_details;
  input_details.set_mime_type("video/h264");
  params.set_input_details(std::move(input_details));
  params.set_require_hw(true);
  fuchsia::media::StreamProcessorPtr processor;
  factory->CreateDecoder(std::move(params), processor.NewRequest());
  bool processor_failed = false;
  processor.set_error_handler([&](zx_status_t status) {
    // This should error out.
    processor_failed = true;
  });

  processor.events().OnInputConstraints = [&](fuchsia::media::StreamBufferConstraints constraints) {
    if (constraints.has_buffer_constraints_version_ordinal()) {
      FAIL() << constraints.buffer_constraints_version_ordinal();
    }
    FAIL() << "fuchsia::media::StreamBufferConstraints{}";
  };

  RunLoopUntil([&]() { return processor_failed || HasFailure(); });
}

TEST_F(Integration, MagmaEncoder) {
  auto builder = RealmBuilder::Create();
  InitializeRoutes(builder);
  auto realm = builder.Build(dispatcher());
  auto cleanup = fit::defer([&]() {
    bool complete = false;
    realm.Teardown([&](fit::result<fuchsia::component::Error> result) { complete = true; });
    RunLoopUntil([&]() { return complete; });
  });
  auto factory = realm.component().Connect<fuchsia::mediacodec::CodecFactory>();

  factory.set_error_handler([&](zx_status_t status) { FAIL() << zx_status_get_string(status); });

  fuchsia::mediacodec::CreateEncoder_Params params;
  fuchsia::media::FormatDetails input_details;
  input_details.set_mime_type("video/h264");
  fuchsia::media::EncoderSettings encoder_settings;
  encoder_settings.set_h264({});
  input_details.set_encoder_settings(std::move(encoder_settings));
  params.set_input_details(std::move(input_details));
  params.set_require_hw(true);
  fuchsia::media::StreamProcessorPtr processor;
  factory->CreateEncoder(std::move(params), processor.NewRequest());
  processor.set_error_handler([&](zx_status_t status) { FAIL() << zx_status_get_string(status); });

  bool on_input_constraints_called = false;
  processor.events().OnInputConstraints = [&](fuchsia::media::StreamBufferConstraints constraints) {
    on_input_constraints_called = true;
    processor.Unbind();
  };

  RunLoopUntil([&]() { return on_input_constraints_called || HasFailure(); });

  magma_device_.CloseAll();

  // Eventually codecs from the device should disappear.
  while (true) {
    fuchsia::mediacodec::CreateEncoder_Params params;
    fuchsia::media::FormatDetails input_details;
    input_details.set_mime_type("video/h264");
    fuchsia::media::EncoderSettings encoder_settings;
    encoder_settings.set_h264({});
    input_details.set_encoder_settings(std::move(encoder_settings));
    params.set_input_details(std::move(input_details));
    params.set_require_hw(true);
    fuchsia::media::StreamProcessorPtr processor;
    factory->CreateEncoder(std::move(params), processor.NewRequest());

    bool processor_failed = false;
    processor.set_error_handler([&](zx_status_t status) { processor_failed = true; });

    bool on_input_constraints_called = false;
    processor.events().OnInputConstraints =
        [&](fuchsia::media::StreamBufferConstraints constraints) {
          on_input_constraints_called = true;
          processor.Unbind();
        };
    RunLoopUntil([&]() { return processor_failed || on_input_constraints_called; });
    if (processor_failed) {
      break;
    }
    // Ignore this success and try again.
  }
}
