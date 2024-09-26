// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_DRIVER_HOST_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_DRIVER_HOST_H_

#include <fidl/fuchsia.driver.host/cpp/fidl.h>
#include <fidl/fuchsia.driver.host/cpp/wire.h>
#include <lib/async/cpp/wait.h>
#include <lib/vfs/cpp/pseudo_dir.h>

#include <fbl/intrusive_double_list.h>

#include "src/devices/bin/driver_loader/loader.h"

namespace driver_manager {

class DriverHost {
 public:
  using StartCallback = fit::callback<void(zx::result<>)>;

  // Components needed to load a driver using dynamic linking.
  struct DriverLoadArgs {
    static zx::result<DriverLoadArgs> Create(
        fuchsia_component_runner::wire::ComponentStartInfo start_info);

    DriverLoadArgs(std::string_view driver_soname, zx::vmo driver_file,
                   fidl::ClientEnd<fuchsia_io::Directory> lib_dir)
        : driver_soname(driver_soname),
          driver_file(std::move(driver_file)),
          lib_dir(std::move(lib_dir)) {}

    std::string driver_soname;
    zx::vmo driver_file;
    fidl::ClientEnd<fuchsia_io::Directory> lib_dir;
  };

  virtual void Start(fidl::ClientEnd<fuchsia_driver_framework::Node> client_end,
                     std::string node_name,
                     fuchsia_driver_framework::wire::NodePropertyDictionary node_properties,
                     fidl::VectorView<fuchsia_driver_framework::wire::NodeSymbol> symbols,
                     fuchsia_component_runner::wire::ComponentStartInfo start_info,
                     fidl::ServerEnd<fuchsia_driver_host::Driver> driver, StartCallback cb) = 0;

  // Loads and starts a driver using dynamic linking.
  virtual void StartWithDynamicLinker(
      fidl::ClientEnd<fuchsia_driver_framework::Node> node, std::string node_name,
      DriverLoadArgs load_args, fidl::ServerEnd<fuchsia_driver_host::Driver> driver_host_server_end,
      StartCallback cb) {
    cb(zx::error(ZX_ERR_NOT_SUPPORTED));
  }

  virtual zx::result<uint64_t> GetProcessKoid() const = 0;
};

class DriverHostComponent final
    : public DriverHost,
      public fbl::DoublyLinkedListable<std::unique_ptr<DriverHostComponent>> {
 public:
  DriverHostComponent(fidl::ClientEnd<fuchsia_driver_host::DriverHost> driver_host,
                      async_dispatcher_t* dispatcher,
                      fbl::DoublyLinkedList<std::unique_ptr<DriverHostComponent>>* driver_hosts,
                      std::shared_ptr<bool> server_connected);

  void Start(fidl::ClientEnd<fuchsia_driver_framework::Node> client_end, std::string node_name,
             fuchsia_driver_framework::wire::NodePropertyDictionary node_properties,
             fidl::VectorView<fuchsia_driver_framework::wire::NodeSymbol> symbols,
             fuchsia_component_runner::wire::ComponentStartInfo start_info,
             fidl::ServerEnd<fuchsia_driver_host::Driver> driver, StartCallback cb) override;

  zx::result<fuchsia_driver_host::ProcessInfo> GetProcessInfo() const;
  zx::result<uint64_t> GetProcessKoid() const override;
  zx::result<uint64_t> GetJobKoid() const;

  zx::result<> InstallLoader(fidl::ClientEnd<fuchsia_ldsvc::Loader> loader_client) const;

 private:
  void InitializeElfDir();

  fidl::WireSharedClient<fuchsia_driver_host::DriverHost> driver_host_;
  mutable std::optional<fuchsia_driver_host::ProcessInfo> process_info_;
  vfs::PseudoDir runtime_dir_;
  async_dispatcher_t* dispatcher_;
  std::shared_ptr<bool> server_connected_;
};

zx::result<> SetEncodedConfig(
    fidl::WireTableBuilder<fuchsia_driver_framework::wire::DriverStartArgs>& args,
    fuchsia_component_runner::wire::ComponentStartInfo& start_info);

// A driver host that has been loaded by dynamic linking.
class DynamicLinkerDriverHostComponent final
    : public DriverHost,
      public fbl::DoublyLinkedListable<std::unique_ptr<DynamicLinkerDriverHostComponent>> {
 public:
  DynamicLinkerDriverHostComponent(
      fidl::ClientEnd<fuchsia_driver_loader::DriverHost> client, async_dispatcher_t* dispatcher,
      zx::channel bootstrap_sender, std::unique_ptr<driver_loader::Loader> loader,
      fbl::DoublyLinkedList<std::unique_ptr<DynamicLinkerDriverHostComponent>>* driver_hosts);

  // Starts listening for |ZX_ERR_PEER_CLOSED| on the bootstrap channel, in which case it will
  // remove itself from the |driver_hosts| list.
  zx_status_t StartBootstrapCloseListener() { return bootstrap_close_listener_.Begin(dispatcher_); }

  void Start(fidl::ClientEnd<fuchsia_driver_framework::Node> client_end, std::string node_name,
             fuchsia_driver_framework::wire::NodePropertyDictionary node_properties,
             fidl::VectorView<fuchsia_driver_framework::wire::NodeSymbol> symbols,
             fuchsia_component_runner::wire::ComponentStartInfo start_info,
             fidl::ServerEnd<fuchsia_driver_host::Driver> driver, StartCallback cb) override {
    cb(zx::error(ZX_ERR_NOT_SUPPORTED));
  }

  void StartWithDynamicLinker(fidl::ClientEnd<fuchsia_driver_framework::Node> node,
                              std::string node_name, DriverLoadArgs load_args,
                              fidl::ServerEnd<fuchsia_driver_host::Driver> driver_host_server_end,
                              StartCallback cb) override;

  zx::result<uint64_t> GetProcessKoid() const override { return zx::error(ZX_ERR_NOT_SUPPORTED); }

  driver_loader::Loader* loader() { return loader_.get(); }

 private:
  async_dispatcher_t* dispatcher_;
  fidl::WireClient<fuchsia_driver_loader::DriverHost> driver_host_loader_;
  zx::channel bootstrap_sender_;
  std::unique_ptr<driver_loader::Loader> loader_;

  async::Wait bootstrap_close_listener_;

  // TODO(https://fxbug.dev/357854682): pass this to the started driver host once
  // fuchsia_driver_host.DriverHost is implemented. Store it here for now so the node doesn't think
  // the driver host has died prematurely.
  std::vector<fidl::ServerEnd<fuchsia_driver_host::Driver>> endpoints_for_driver_hosts_;
};

}  // namespace driver_manager

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_DRIVER_HOST_H_
