// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v2/driver_host.h"

#include <lib/driver/component/cpp/internal/start_args.h>

#include <memory>

#include "lib/vfs/cpp/pseudo_file.h"
#include "src/devices/lib/log/log.h"
#include "src/lib/fsl/handles/object_info.h"

namespace fdh = fuchsia_driver_host;
namespace frunner = fuchsia_component_runner;
namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf
namespace dfv2 {

namespace {
std::unique_ptr<vfs::PseudoFile> CreateReadonlyFile(
    fit::function<zx::result<std::string>()> content_producer) {
  auto read_fn = [content_producer = std::move(content_producer)](std::vector<uint8_t>* output,
                                                                  size_t max_file_size) {
    zx::result<std::string> contents = content_producer();
    if (contents.is_error()) {
      return contents.status_value();
    }
    output->resize(contents->length());
    std::copy(contents->begin(), contents->end(), output->begin());
    return ZX_OK;
  };

  return std::make_unique<vfs::PseudoFile>(30, std::move(read_fn));
}
}  // namespace

zx::result<> SetEncodedConfig(fidl::WireTableBuilder<fdf::wire::DriverStartArgs>& args,
                              frunner::wire::ComponentStartInfo& start_info) {
  if (!start_info.has_encoded_config()) {
    return zx::ok();
  }

  if (!start_info.encoded_config().is_buffer() && !start_info.encoded_config().is_bytes()) {
    LOGF(ERROR, "Failed to parse encoded config in start info. Encoding is not buffer or bytes.");
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  if (start_info.encoded_config().is_buffer()) {
    args.config(std::move(start_info.encoded_config().buffer().vmo));
    return zx::ok();
  }

  auto vmo_size = start_info.encoded_config().bytes().count();
  zx::vmo vmo;

  auto status = zx::vmo::create(vmo_size, ZX_RIGHT_TRANSFER | ZX_RIGHT_READ, &vmo);
  if (status != ZX_OK) {
    return zx::error(status);
  }

  status = vmo.write(start_info.encoded_config().bytes().data(), 0, vmo_size);
  if (status != ZX_OK) {
    return zx::error(status);
  }

  args.config(std::move(vmo));
  return zx::ok();
}

DriverHostComponent::DriverHostComponent(
    fidl::ClientEnd<fdh::DriverHost> driver_host, async_dispatcher_t* dispatcher,
    fbl::DoublyLinkedList<std::unique_ptr<DriverHostComponent>>* driver_hosts)
    : driver_host_(std::move(driver_host), dispatcher,
                   fidl::ObserveTeardown([this, driver_hosts] { driver_hosts->erase(*this); })),
      dispatcher_(dispatcher) {
  InitializeElfDir();
}

void DriverHostComponent::InitializeElfDir() {
  // This directory allows zxdb to conn
  auto elf_dir = std::make_unique<vfs::PseudoDir>();

  auto now = std::to_string(zx::clock::get_monotonic().get());
  elf_dir->AddEntry("process_start_time", CreateReadonlyFile([now]() { return zx::ok(now); }));

  elf_dir->AddEntry("job_id", CreateReadonlyFile([this]() -> zx::result<std::string> {
                      zx::result job_id = GetJobKoid();
                      if (job_id.is_error()) {
                        return job_id.take_error();
                      }
                      return zx::ok(std::to_string(*job_id));
                    }));

  elf_dir->AddEntry("process_id", CreateReadonlyFile([this]() -> zx::result<std::string> {
                      zx::result process_id = GetProcessKoid();
                      if (process_id.is_error()) {
                        return process_id.take_error();
                      }
                      return zx::ok(std::to_string(*process_id));
                    }));

  runtime_dir_.AddEntry("elf", std::move(elf_dir));
}

void DriverHostComponent::Start(
    fidl::ClientEnd<fdf::Node> client_end, std::string node_name,
    fidl::VectorView<fuchsia_driver_framework::wire::NodeSymbol> symbols,
    frunner::wire::ComponentStartInfo start_info,
    fidl::ServerEnd<fuchsia_driver_host::Driver> driver, StartCallback cb) {
  auto binary = fdf_internal::ProgramValue(start_info.program(), "binary").value_or("");
  fidl::Arena arena;
  auto args = fdf::wire::DriverStartArgs::Builder(arena);
  args.node(std::move(client_end))
      .node_name(fidl::StringView::FromExternal(node_name))
      .url(start_info.resolved_url())
      .program(start_info.program())
      .incoming(start_info.ns())
      .outgoing_dir(std::move(start_info.outgoing_dir()));

  auto status = SetEncodedConfig(args, start_info);
  if (status.is_error()) {
    cb(status.take_error());
    return;
  }

  if (!symbols.empty()) {
    args.symbols(symbols);
  }

  if (start_info.has_runtime_dir()) {
    runtime_dir_.Serve(fuchsia::io::OpenFlags::RIGHT_READABLE,
                       start_info.runtime_dir().TakeChannel(), dispatcher_);
  }

  driver_host_->Start(args.Build(), std::move(driver))
      .ThenExactlyOnce([cb = std::move(cb), binary = std::move(binary)](auto& result) mutable {
        if (!result.ok()) {
          LOGF(ERROR, "Failed to start driver '%s' in driver host: %s", binary.c_str(),
               result.FormatDescription().c_str());
          cb(zx::error(result.status()));
          return;
        }
        if (result->is_error()) {
          LOGF(ERROR, "Failed to start driver '%s' in driver host: %s", binary.c_str(),
               zx_status_get_string(result->error_value()));
          cb(result->take_error());
          return;
        }
        cb(zx::ok());
      });
}

zx::result<fuchsia_driver_host::ProcessInfo> DriverHostComponent::GetProcessInfo() const {
  if (process_info_) {
    return zx::ok(*process_info_);
  }
  fidl::WireResult result = driver_host_.sync()->GetProcessInfo();
  if (!result.ok()) {
    return zx::error(result.status());
  }
  if (result->is_error()) {
    return zx::error(result->error_value());
  }
  process_info_ = fidl::ToNatural(*result->value());
  return zx::ok(*process_info_);
}

zx::result<uint64_t> DriverHostComponent::GetJobKoid() const {
  zx::result result = GetProcessInfo();
  if (result.is_error()) {
    return result.take_error();
  }
  return zx::ok(result->job_koid());
}

zx::result<uint64_t> DriverHostComponent::GetProcessKoid() const {
  zx::result result = GetProcessInfo();
  if (result.is_error()) {
    return result.take_error();
  }
  return zx::ok(result->process_koid());
}

zx::result<> DriverHostComponent::InstallLoader(
    fidl::ClientEnd<fuchsia_ldsvc::Loader> loader_client) const {
  auto result = driver_host_->InstallLoader(std::move(loader_client));
  if (!result.ok()) {
    return zx::error(result.status());
  }
  return zx::ok();
}

}  // namespace dfv2
