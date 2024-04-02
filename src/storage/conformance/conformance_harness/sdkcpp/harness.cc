// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.io.test/cpp/fidl.h>
#include <fidl/fuchsia.io/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/default.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/vfs/cpp/pseudo_dir.h>
#include <lib/vfs/cpp/pseudo_file.h>
#include <lib/vfs/cpp/remote_dir.h>
#include <lib/vfs/cpp/vmo_file.h>
#include <zircon/status.h>

#include <cstdint>
#include <limits>
#include <memory>
#include <vector>

namespace fio_test = fuchsia_io_test;

zx_status_t DummyWriter(const std::vector<uint8_t>&) { return ZX_OK; }

class SdkCppHarness : public fidl::Server<fio_test::Io1Harness> {
 public:
  explicit SdkCppHarness() = default;

  ~SdkCppHarness() override = default;

  void GetConfig(GetConfigCompleter::Sync& completer) final {
    fio_test::Io1Config config;

    // The SDK VFS uses the in-tree C++ VFS under the hood, and thus should support at *least* the
    // same feature set. Other than adding additional supported options, the remainder of this
    // test harness should be the exact same as the current SDK VFS one.

    // Supported options:
    config.mutable_file(true);
    config.supports_vmo_file(true);
    config.supports_get_backing_memory(true);
    config.supports_remote_dir(true);
    config.supports_get_token(true);

    // Unsupported options:
    config.supports_create(false);
    config.supports_executable_file(false);
    config.supports_rename(false);
    config.supports_link(false);
    config.supports_set_attr(false);
    config.supports_unlink(false);
    config.supports_get_attributes(false);
    config.supports_update_attributes(false);
    config.supports_directory_watchers(false);

    completer.Reply(std::move(config));
  }

  void GetDirectory(GetDirectoryRequest& request, GetDirectoryCompleter::Sync& completer) final {
    auto dir = std::make_unique<vfs::PseudoDir>();

    if (request.root().entries()) {
      for (auto& entry : *request.root().entries()) {
        AddEntry(std::move(*entry), *dir);
      }
    }

    // TODO(https://fxbug.dev/29393642): Support the new C++ bindings in the SDK VFS so that we can
    // use `fuchsia_io::OpenFlags` instead of the deprecated HLCPP `fuchsia::io::OpenFlags` type.
    fuchsia::io::OpenFlags flags = fuchsia::io::OpenFlags{static_cast<uint32_t>(request.flags())};
    ZX_ASSERT_MSG(dir->Serve(flags, request.directory_request().TakeChannel()) == ZX_OK,
                  "Failed to serve directory!");
    directories_.push_back(std::move(dir));
  }

 private:
  // NOLINTNEXTLINE(misc-no-recursion): Test-only code, recursion is acceptable here.
  void AddEntry(fio_test::DirectoryEntry entry, vfs::PseudoDir& dest) {
    switch (entry.Which()) {
      case fio_test::DirectoryEntry::Tag::kDirectory: {
        fio_test::Directory directory = std::move(entry.directory().value());
        auto dir_entry = std::make_unique<vfs::PseudoDir>();
        if (directory.entries()) {
          for (auto& child_entry : *directory.entries()) {
            AddEntry(std::move(*child_entry), *dir_entry);
          }
        }
        ZX_ASSERT_MSG(dest.AddEntry(*directory.name(), std::move(dir_entry)) == ZX_OK,
                      "Failed to add Directory entry!");
        break;
      }
      case fio_test::DirectoryEntry::Tag::kRemoteDirectory: {
        fio_test::RemoteDirectory remote_directory = std::move(entry.remote_directory().value());

        // TODO(https://fxbug.dev/29393642): Support the new C++ bindings in the SDK VFS so we can
        // construct a `vfs::RemoteDir` using a `fidl::ClientEnd` directly.
        auto remote_dir_entry =
            std::make_unique<vfs::RemoteDir>(remote_directory.remote_client()->TakeChannel());
        dest.AddEntry(*remote_directory.name(), std::move(remote_dir_entry));
        break;
      }
      case fio_test::DirectoryEntry::Tag::kFile: {
        fio_test::File file = std::move(entry.file().value());
        std::vector<uint8_t> contents = std::move(*file.contents());
        auto read_handler = [contents = std::move(contents)](std::vector<uint8_t>* output,
                                                             size_t max_bytes) -> zx_status_t {
          ZX_ASSERT(contents.size() <= max_bytes);
          *output = std::vector<uint8_t>(contents);
          return ZX_OK;
        };
        auto file_entry = std::make_unique<vfs::PseudoFile>(std::numeric_limits<size_t>::max(),
                                                            read_handler, &DummyWriter);
        ZX_ASSERT_MSG(dest.AddEntry(*file.name(), std::move(file_entry)) == ZX_OK,
                      "Failed to add File entry!");
        break;
      }
      case fio_test::DirectoryEntry::Tag::kVmoFile: {
        fio_test::VmoFile vmo_file = std::move(entry.vmo_file().value());
        zx::vmo& vmo = *vmo_file.vmo();
        uint64_t size;
        zx_status_t status = vmo.get_prop_content_size(&size);
        ZX_ASSERT_MSG(status == ZX_OK, "%s", zx_status_get_string(status));
        auto vmo_file_entry = std::make_unique<vfs::VmoFile>(std::move(vmo), size,
                                                             vfs::VmoFile::WriteOption::WRITABLE);
        ZX_ASSERT_MSG(dest.AddEntry(*vmo_file.name(), std::move(vmo_file_entry)) == ZX_OK,
                      "Failed to add VmoFile entry!");
        break;
      }
      case fio_test::DirectoryEntry::Tag::kExecutableFile:
        ZX_PANIC("Executable files are not supported!");
        break;
    }
  }

  std::vector<std::unique_ptr<vfs::PseudoDir>> directories_;
};

int main(int argc, const char** argv) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  fuchsia_logging::SetTags({"io_conformance_harness_sdkcpp_new"});
  component::OutgoingDirectory outgoing(loop.dispatcher());
  zx::result result = outgoing.AddProtocol<fio_test::Io1Harness>(std::make_unique<SdkCppHarness>());
  ZX_ASSERT_MSG(result.is_ok(), "Failed to add protocol: %s", result.status_string());
  result = outgoing.ServeFromStartupInfo();
  ZX_ASSERT_MSG(result.is_ok(), "Failed to serve outgoing directory: %s", result.status_string());
  return loop.Run();
}
