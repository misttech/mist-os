// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_DEVFS_DEVFS_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_DEVFS_DEVFS_H_

#include <fidl/fuchsia.device.fs/cpp/wire.h>
#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/async/dispatcher.h>
#include <lib/component/incoming/cpp/clone.h>

#include <random>

#include <fbl/ref_ptr.h>
#include <fbl/string.h>

#include "src/storage/lib/vfs/cpp/pseudo_dir.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace driver_manager {

class Devfs;
class PseudoDir;
class DevfsDevice;

class Devnode {
 public:
  // This class represents a device in devfs. It is called "passthrough" because it sends
  // the channel and the connection type to a callback function.
  struct PassThrough {
    // The Device connect callback is for accessing the /dev/class/xxx protocol for the device
    using DeviceConnectCallback = fit::function<zx_status_t(zx::channel)>;
    // The controller callback is for accessing the fuchsia.device/Controller
    // interface associated with the device.
    using ControllerConnectCallback =
        fit::function<zx_status_t(fidl::ServerEnd<fuchsia_device::Controller>)>;
    // Create a Passthrough class. The client must make sure that any captures in the callback
    // live as long as the passthrough class (for this reason it's strongly recommended to use
    // owned captures).
    explicit PassThrough(DeviceConnectCallback device_callback,
                         ControllerConnectCallback controller_callback)
        : device_connect(std::make_shared<DeviceConnectCallback>(std::move(device_callback))),
          controller_connect(
              std::make_shared<ControllerConnectCallback>(std::move(controller_callback))) {}

    PassThrough Clone() { return *this; }

    std::shared_ptr<DeviceConnectCallback> device_connect;
    std::shared_ptr<ControllerConnectCallback> controller_connect;
  };

  using Target = std::optional<PassThrough>;

  // Constructs a root node.
  explicit Devnode(Devfs& devfs);

  // `parent` must outlive `this`.
  Devnode(Devfs& devfs, PseudoDir& parent, Target target, fbl::String name);

  ~Devnode();

  Devnode(const Devnode&) = delete;
  Devnode& operator=(const Devnode&) = delete;

  Devnode(Devnode&&) = delete;
  Devnode& operator=(Devnode&&) = delete;

  // Add a child to this Devnode. The child will be added to both the topological path and under the
  // given `class_name`.
  zx_status_t add_child(std::string_view name, std::optional<std::string_view> class_name,
                        Target target, DevfsDevice& out_child);

  // Exports `target`.
  //
  // If `topological_path` is provided, then `target` will be exported at that path under `this`.
  //
  // If `class_path` is provided, then `target` will be exported under that class path.
  zx_status_t export_dir(Devnode::Target target, std::optional<std::string_view> topological_path,
                         std::optional<std::string_view> class_path,
                         std::vector<std::unique_ptr<Devnode>>& out);

  std::string_view name() const;
  PseudoDir& children() const { return node().children(); }
  void advertise_modified();

  // Publishes the node to devfs. Asserts if called more than once.
  void publish();

  // The actual vnode implementation. This is distinct from the outer class
  // because `fs::Vnode` imposes reference-counted semantics, and we want to
  // preserve owned semantics on the outer class.
  //
  // This is exposed for use in tests.
  class VnodeImpl : public fs::Vnode {
   public:
    fuchsia_io::NodeProtocolKinds GetProtocols() const final;
    zx::result<fs::VnodeAttributes> GetAttributes() const final;
    zx_status_t Lookup(std::string_view name, fbl::RefPtr<fs::Vnode>* out) final;
    zx_status_t WatchDir(fs::FuchsiaVfs* vfs, fuchsia_io::wire::WatchMask mask, uint32_t options,
                         fidl::ServerEnd<fuchsia_io::DirectoryWatcher> watcher) final;
    zx_status_t Readdir(fs::VdirCookie* cookie, void* dirents, size_t len,
                        size_t* out_actual) final;
    zx_status_t ConnectService(zx::channel channel) final;

    PseudoDir& children() const { return *children_; }

    Devnode& holder_;
    const Target target_;

   private:
    friend fbl::internal::MakeRefCountedHelper<VnodeImpl>;

    VnodeImpl(Devnode& holder, Target target);

    bool IsDirectory() const;

    fbl::RefPtr<PseudoDir> children_ = fbl::MakeRefCounted<PseudoDir>();
  };

 private:
  zx_status_t export_class(Devnode::Target target, std::string_view class_path,
                           std::vector<std::unique_ptr<Devnode>>& out);

  zx_status_t export_topological_path(Devnode::Target target, std::string_view topological_path,
                                      std::vector<std::unique_ptr<Devnode>>& out);

  VnodeImpl& node() const { return *node_; }
  const Target& target() const { return node_->target_; }

  friend class Devfs;
  friend class PseudoDir;

  Devfs& devfs_;

  fbl::RefPtr<PseudoDir> parent_;

  const fbl::RefPtr<VnodeImpl> node_;

  const std::optional<fbl::String> name_;
};

class PseudoDir : public fs::PseudoDir {
 public:
  std::unordered_map<fbl::String, std::reference_wrapper<Devnode>, std::hash<std::string_view>>
      unpublished;
};

class DevfsDevice {
 public:
  void advertise_modified();
  void publish();
  void unpublish();

  std::optional<Devnode>& protocol_node() { return protocol_; }
  std::optional<Devnode>& topological_node() { return topological_; }

 private:
  std::optional<Devnode> topological_;
  // TODO(https://fxbug.dev/42062564): These protocol nodes are currently always empty directories.
  // Change this to a pure `RemoteNode` that doesn't expose a directory.
  std::optional<Devnode> protocol_;
};

class Devfs {
 public:
  // `root` must outlive `this`.
  explicit Devfs(std::optional<Devnode>& root);

  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> Connect(fs::FuchsiaVfs& vfs);

  zx::result<std::string> MakeInstanceName(std::string_view class_name);

  fbl::RefPtr<PseudoDir> get_class_entry(std::string_view class_name) {
    EnsureClassExists(class_name);
    return class_entries_[std::string(class_name)];
  }

 private:
  friend class Devnode;

  static std::optional<std::reference_wrapper<fs::Vnode>> Lookup(PseudoDir& parent,
                                                                 std::string_view name);
  void EnsureClassExists(std::string_view name);

  Devnode& root_;
  std::default_random_engine device_number_generator_;

  fbl::RefPtr<PseudoDir> class_ = fbl::MakeRefCounted<PseudoDir>();
  std::unordered_map<std::string, fbl::RefPtr<PseudoDir>> class_entries_;
};

}  // namespace driver_manager

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_DEVFS_DEVFS_H_
