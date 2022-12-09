// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_MISC_DRIVERS_COMPAT_DEVFS_VNODE_H_
#define SRC_DEVICES_MISC_DRIVERS_COMPAT_DEVFS_VNODE_H_

#include <fidl/fuchsia.device/cpp/wire.h>
#include <lib/fidl/cpp/wire/transaction.h>

#include <variant>

#include <ddktl/fidl.h>

#include "src/devices/lib/fidl/device_server.h"
#include "src/lib/storage/vfs/cpp/vnode.h"

class DevfsVnode : public fs::Vnode {
 public:
  // `dev` must outlive `DevfsVnode`.
  DevfsVnode(devfs_fidl::DeviceInterface* dev, async_dispatcher_t* dispatcher)
      : dev_(dev), dispatcher_(dispatcher) {}

  void ConnectToDeviceFidl(zx::channel server);

  // fs::Vnode methods
  zx_status_t GetAttributes(fs::VnodeAttributes* a) override;
  fs::VnodeProtocolSet GetProtocols() const override;
  zx_status_t GetNodeInfoForProtocol(fs::VnodeProtocol protocol, fs::Rights rights,
                                     fs::VnodeRepresentation* info) override;
  void HandleFsSpecificMessage(fidl::IncomingHeaderAndMessage& msg,
                               fidl::Transaction* txn) override;

  devfs_fidl::DeviceInterface* dev() { return dev_; }

 private:
  devfs_fidl::DeviceInterface* const dev_;
  async_dispatcher_t* const dispatcher_;
};

#endif  // SRC_DEVICES_MISC_DRIVERS_COMPAT_DEVFS_VNODE_H_
