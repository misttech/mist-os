// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_DEVFS_DEVFS_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_DEVFS_DEVFS_H_

#include <fidl/fuchsia.device.fs/cpp/wire.h>
#include <fidl/fuchsia.device.manager/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/async/dispatcher.h>
#include <lib/component/incoming/cpp/clone.h>

#include <variant>

#include <fbl/ref_ptr.h>
#include <fbl/string.h>

#include "src/lib/storage/vfs/cpp/pseudo_dir.h"
#include "src/lib/storage/vfs/cpp/vnode.h"

class Devfs;
class PseudoDir;
class DevfsDevice;

std::optional<std::string_view> ProtocolIdToClassName(uint32_t protocol_id);

class Devnode {
 public:
  using ExportOptions = fuchsia_device_fs::wire::ExportOptions;
  struct NoRemote {
    mutable ExportOptions export_options;
  };
  struct Service {
    zx::result<Service> Clone() {
      zx::result result = component::Clone(remote);
      if (result.is_error()) {
        return result.take_error();
      }
      return zx::ok(Service{
          .remote = std::move(result.value()),
          .path = path,
          .export_options = export_options,
      });
    }
    fidl::ClientEnd<fuchsia_io::Directory> remote;
    std::string path;
    mutable ExportOptions export_options;
  };
  struct Connector {
    fidl::WireSharedClient<fuchsia_device_fs::Connector> connector;
    mutable ExportOptions export_options;
    Connector Clone() {
      return Connector{
          .connector = connector.Clone(),
          .export_options = export_options,
      };
    }
  };
  struct Remote {
    fidl::WireSharedClient<fuchsia_device_manager::DeviceController> connector;

    Remote Clone() {
      return Remote{
          .connector = connector.Clone(),
      };
    }
  };

  using Target = std::variant<NoRemote, Service, Remote, Connector>;

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
  ExportOptions export_options() const;
  ExportOptions* export_options();
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
    fs::VnodeProtocolSet GetProtocols() const final;
    zx_status_t GetAttributes(fs::VnodeAttributes* a) final;
    zx_status_t Lookup(std::string_view name, fbl::RefPtr<fs::Vnode>* out) final;
    zx_status_t WatchDir(fs::Vfs* vfs, fuchsia_io::wire::WatchMask mask, uint32_t options,
                         fidl::ServerEnd<fuchsia_io::DirectoryWatcher> watcher) final;
    zx_status_t Readdir(fs::VdirCookie* cookie, void* dirents, size_t len,
                        size_t* out_actual) final;
    zx_status_t GetNodeInfoForProtocol(fs::VnodeProtocol protocol, fs::Rights rights,
                                       fs::VnodeRepresentation* info) final;
    zx_status_t ConnectService(zx::channel channel) final;
    bool IsService() const final;
    zx_status_t OpenNode(ValidatedOptions options, fbl::RefPtr<Vnode>* out_redirect) final;

    PseudoDir& children() const { return *children_; }

    Devnode& holder_;
    const Target target_;

   private:
    friend fbl::internal::MakeRefCountedHelper<VnodeImpl>;

    VnodeImpl(Devnode& holder, Target target);

    bool IsDirectory() const;

    class RemoteNode : public fs::Vnode {
     private:
      friend fbl::internal::MakeRefCountedHelper<RemoteNode>;

      explicit RemoteNode(VnodeImpl& parent) : parent_(parent) {}

      fs::VnodeProtocolSet GetProtocols() const final;
      zx_status_t GetNodeInfoForProtocol(fs::VnodeProtocol protocol, fs::Rights rights,
                                         fs::VnodeRepresentation* info) final;

      bool IsRemote() const final;
      zx_status_t OpenRemote(fuchsia_io::OpenFlags, uint32_t, fidl::StringView,
                             fidl::ServerEnd<fuchsia_io::Node>) const final;

      VnodeImpl& parent_;
    };

    fbl::RefPtr<PseudoDir> children_ = fbl::MakeRefCounted<PseudoDir>();
    fbl::RefPtr<RemoteNode> remote_ = fbl::MakeRefCounted<RemoteNode>(*this);
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

// Represents an entry in the /dev/class directory. A thin wrapper around
// `PseudoDir` that can be owned and keeps track of numbered entries
// contained within it.
class ProtoNode {
 public:
  ProtoNode(fbl::String name, uint32_t initial_device_number);

  ProtoNode(const ProtoNode&) = delete;
  ProtoNode& operator=(const ProtoNode&) = delete;

  ProtoNode(ProtoNode&&) = delete;
  ProtoNode& operator=(ProtoNode&&) = delete;

  // This is used in tests only.
  std::string_view name() const { return name_; }
  PseudoDir& children() const { return *children_; }

 private:
  friend class Devfs;
  friend class Devnode;

  zx::result<fbl::String> seq_name();

  const fbl::String name_;

  uint32_t next_device_number_;

  fbl::RefPtr<PseudoDir> children_ = fbl::MakeRefCounted<PseudoDir>();
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
  // TODO(https://fxbug.dev/111253): These protocol nodes are currently always empty directories.
  // Change this to a pure `RemoteNode` that doesn't expose a directory.
  std::optional<Devnode> protocol_;
};

class Devfs {
 public:
  // `root` must outlive `this`.
  explicit Devfs(std::optional<Devnode>& root,
                 std::optional<fidl::ClientEnd<fuchsia_io::Directory>> diagnostics = {});

  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> Connect(fs::FuchsiaVfs& vfs);

  // This method is exposed for testing.
  std::optional<std::reference_wrapper<ProtoNode>> proto_node(std::string_view protocol_name);
  std::optional<std::reference_wrapper<ProtoNode>> proto_node(uint32_t protocol_id);

 private:
  friend class Devnode;

  static std::optional<std::reference_wrapper<fs::Vnode>> Lookup(PseudoDir& parent,
                                                                 std::string_view name);

  Devnode& root_;

  fbl::RefPtr<PseudoDir> class_ = fbl::MakeRefCounted<PseudoDir>();
  std::unordered_map<uint32_t, ProtoNode> proto_info_nodes;
};

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_DEVFS_DEVFS_H_
