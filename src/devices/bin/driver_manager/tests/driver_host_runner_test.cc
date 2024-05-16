// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/driver_host_runner.h"

#include <fidl/fuchsia.component.decl/cpp/test_base.h>
#include <fidl/fuchsia.component/cpp/test_base.h>
#include <fuchsia/io/cpp/fidl_test_base.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fdio/directory.h>
#include <lib/fidl/cpp/binding.h>
#include <zircon/errors.h>

#include <gtest/gtest.h>

#include "src/devices/bin/driver_manager/tests/driver_runner_test_fixture.h"

namespace {

namespace fcomponent = fuchsia_component;
namespace fdata = fuchsia_data;
namespace fdecl = fuchsia_component_decl;
namespace fio = fuchsia::io;
namespace frunner = fuchsia_component_runner;

class TestTransaction : public fidl::Transaction {
 private:
  std::unique_ptr<Transaction> TakeOwnership() override {
    return std::make_unique<TestTransaction>();
  }

  zx_status_t Reply(fidl::OutgoingMessage* message, fidl::WriteOptions write_options) override {
    EXPECT_TRUE(false);
    return ZX_OK;
  }

  void Close(zx_status_t epitaph) override { EXPECT_TRUE(false); }
};

class TestFile : public fio::testing::File_TestBase {
 public:
  explicit TestFile(std::string_view path) : path_(std::move(path)) {}

 private:
  void GetBackingMemory(fio::VmoFlags flags, GetBackingMemoryCallback callback) override {
    EXPECT_EQ(fio::VmoFlags::READ | fio::VmoFlags::EXECUTE | fio::VmoFlags::PRIVATE_CLONE, flags);
    auto endpoints = fidl::Endpoints<fuchsia_io::File>::Create();
    EXPECT_EQ(ZX_OK, fdio_open(path_.data(),
                               static_cast<uint32_t>(fio::OpenFlags::RIGHT_READABLE |
                                                     fio::OpenFlags::RIGHT_EXECUTABLE),
                               endpoints.server.channel().release()));

    fidl::WireSyncClient<fuchsia_io::File> file(std::move(endpoints.client));
    fidl::WireResult result = file->GetBackingMemory(fuchsia_io::wire::VmoFlags(uint32_t(flags)));
    EXPECT_TRUE(result.ok()) << result.FormatDescription();
    auto* res = result.Unwrap();
    if (res->is_error()) {
      callback(fio::File_GetBackingMemory_Result::WithErr(std::move(res->error_value())));
      return;
    }
    callback(fio::File_GetBackingMemory_Result::WithResponse(
        fio::File_GetBackingMemory_Response(std::move(res->value()->vmo))));
  }

  void NotImplemented_(const std::string& name) override {
    printf("Not implemented: File::%s\n", name.data());
  }

  std::string_view path_;
};

class TestDirectory : public fio::testing::Directory_TestBase {
 public:
  using OpenHandler = fit::function<void(fio::OpenFlags flags, std::string path,
                                         fidl::InterfaceRequest<fio::Node> object)>;

  void SetOpenHandler(OpenHandler open_handler) { open_handler_ = std::move(open_handler); }

 private:
  void Open(fio::OpenFlags flags, fio::ModeType mode, std::string path,
            fidl::InterfaceRequest<fio::Node> object) override {
    open_handler_(flags, std::move(path), std::move(object));
  }

  void NotImplemented_(const std::string& name) override {
    printf("Not implemented: Directory::%s\n", name.data());
  }

  OpenHandler open_handler_;
};

class DriverHostRunnerTest : public gtest::TestLoopFixture {
 protected:
  fidl::ClientEnd<fuchsia_component::Realm> ConnectToRealm();
  void CallComponentStart(driver_manager::DriverHostRunner& driver_host_runner);

  driver_runner::TestRealm& realm() { return realm_; }

 private:
  driver_runner::TestRealm realm_;
  std::optional<fidl::ServerBinding<fuchsia_component::Realm>> realm_binding_;
};

fidl::ClientEnd<fuchsia_component::Realm> DriverHostRunnerTest::ConnectToRealm() {
  auto realm_endpoints = fidl::Endpoints<fcomponent::Realm>::Create();
  realm_binding_.emplace(dispatcher(), std::move(realm_endpoints.server), &realm_,
                         fidl::kIgnoreBindingClosure);
  return std::move(realm_endpoints.client);
}

void DriverHostRunnerTest::CallComponentStart(
    driver_manager::DriverHostRunner& driver_host_runner) {
  async::Loop dir_loop{&kAsyncLoopConfigNoAttachToCurrentThread};
  ASSERT_EQ(ZX_OK, dir_loop.StartThread());

  fidl::Arena arena;

  fidl::VectorView<fdata::wire::DictionaryEntry> program_entries(arena, 1);
  program_entries[0].key.Set(arena, "binary");
  program_entries[0].value = fdata::wire::DictionaryValue::WithStr(arena, "bin/driver_host2");
  auto program_builder = fdata::wire::Dictionary::Builder(arena);
  program_builder.entries(program_entries);

  auto pkg_endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();

  fidl::VectorView<frunner::wire::ComponentNamespaceEntry> ns_entries(arena, 1);
  ns_entries[0] = frunner::wire::ComponentNamespaceEntry::Builder(arena)
                      .path("/pkg")
                      .directory(std::move(pkg_endpoints.client))
                      .Build();

  TestFile file("/pkg/bin/driver_host2");
  fidl::Binding<fio::File> file_binding(&file);
  TestDirectory pkg_directory;
  fidl::Binding<fio::Directory> pkg_binding(&pkg_directory);
  pkg_binding.Bind(pkg_endpoints.server.TakeChannel(), dir_loop.dispatcher());
  pkg_directory.SetOpenHandler(
      [&dir_loop, &file_binding](fio::OpenFlags flags, std::string path, auto object) {
        EXPECT_EQ(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE, flags);
        EXPECT_EQ("bin/driver_host2", path);
        file_binding.Bind(object.TakeChannel(), dir_loop.dispatcher());
      });

  auto start_info_builder = frunner::wire::ComponentStartInfo::Builder(arena);
  start_info_builder.resolved_url("fuchsia-boot:///driver_host2#meta/driver_host2.cm")
      .program(program_builder.Build())
      .ns(ns_entries)
      .numbered_handles(realm().TakeHandles(arena));

  auto controller_endpoints = fidl::Endpoints<frunner::ComponentController>::Create();
  TestTransaction transaction;
  {
    fidl::WireServer<frunner::ComponentRunner>::StartCompleter::Sync completer(&transaction);
    fidl::WireRequest<frunner::ComponentRunner::Start> request{
        start_info_builder.Build(), std::move(controller_endpoints.server)};
    static_cast<fidl::WireServer<frunner::ComponentRunner>&>(driver_host_runner)
        .Start(&request, completer);
  }

  dir_loop.Quit();
  dir_loop.JoinThreads();
}

TEST_F(DriverHostRunnerTest, Start) {
  constexpr std::string_view kDriverHostName = "driver-host-new-";
  constexpr std::string_view kCollection = "driver-hosts";
  constexpr std::string_view kComponentUrl = "fuchsia-boot:///driver_host2#meta/driver_host2.cm";

  bool created_component;
  realm().SetCreateChildHandler(
      [&](fdecl::CollectionRef collection, fdecl::Child decl, std::vector<fdecl::Offer> offers) {
        EXPECT_EQ(kDriverHostName, decl.name().value().substr(0, kDriverHostName.size()));
        EXPECT_EQ(kCollection, collection.name());
        EXPECT_EQ(kComponentUrl, decl.url());
        created_component = true;
      });

  driver_manager::DriverHostRunner driver_host_runner(dispatcher(), ConnectToRealm());
  auto res = driver_host_runner.StartDriverHost();
  ASSERT_EQ(ZX_OK, res.status_value());

  ASSERT_TRUE(RunLoopUntilIdle());
  ASSERT_TRUE(created_component);

  ASSERT_NO_FATAL_FAILURE(CallComponentStart(driver_host_runner));
}

}  // namespace
