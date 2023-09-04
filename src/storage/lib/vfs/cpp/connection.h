// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_LIB_VFS_CPP_CONNECTION_H_
#define SRC_STORAGE_LIB_VFS_CPP_CONNECTION_H_

#ifndef __Fuchsia__
#error "Fuchsia-only header"
#endif

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/async/cpp/wait.h>
#include <lib/async/dispatcher.h>
#include <lib/fidl/cpp/wire/transaction.h>
#include <lib/fit/function.h>
#include <lib/zx/channel.h>
#include <lib/zx/event.h>
#include <lib/zx/result.h>
#include <zircon/fidl.h>

#include <memory>

#include <fbl/intrusive_double_list.h>
#include <fbl/ref_ptr.h>

#include "src/storage/lib/vfs/cpp/fuchsia_vfs.h"
#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace fs {

namespace internal {

class Binding;

// Perform basic flags sanitization.
// Returns false if the flags combination is invalid.
bool PrevalidateFlags(fuchsia_io::wire::OpenFlags flags);

zx_status_t EnforceHierarchicalRights(Rights parent_rights, VnodeConnectionOptions child_options,
                                      VnodeConnectionOptions* out_options);

// Connection is a base class representing an open connection to a Vnode (the server-side component
// of a file descriptor). It contains the logic to synchronize connection teardown with the vfs, as
// well as shared utilities such as connection cloning and enforcement of connection rights.
// Connections will be managed in a |fbl::DoublyLinkedList|.
//
// This class does not implement any FIDL generated C++ interfaces per se. Rather, each
// |fuchsia.io/{Node, File, Directory, ...}| protocol is handled by a separate corresponding
// subclass, potentially delegating shared functionalities back here.
//
// The Vnode's methods will be invoked in response to FIDL protocol messages received over the
// channel.
//
// This class is thread-safe.
class Connection : public fbl::DoublyLinkedListable<std::unique_ptr<Connection>> {
 public:
  // Closes the connection.
  //
  // The connection must not be destroyed if its wait handler is running concurrently on another
  // thread.
  //
  // In practice, this means the connection must have already been remotely closed, or it must be
  // destroyed on the wait handler's dispatch thread to prevent a race.
  virtual ~Connection();

  // Triggers asynchronous closure of the receiver.
  void Unbind();

  using OnUnbound = fit::function<void(Connection*)>;

  // Begins waiting for messages on the channel. |channel| is the channel on which the FIDL protocol
  // will be served.
  //
  // Before calling this function, the connection ownership must be transferred to the Vfs through
  // |RegisterConnection|. Cannot be called more than once in the lifetime of the connection.
  void StartDispatching(zx::channel channel, OnUnbound on_unbound);

  fbl::RefPtr<fs::Vnode>& vnode() { return vnode_; }

  zx::result<VnodeRepresentation> GetNodeRepresentation() { return NodeDescribe(); }

 protected:
  virtual std::unique_ptr<Binding> Bind(async_dispatcher*, zx::channel, OnUnbound) = 0;
  // Create a connection bound to a particular vnode.
  //
  // The VFS will be notified when remote side closes the connection.
  //
  // |vfs| is the VFS which is responsible for dispatching operations to the vnode.
  // |vnode| is the vnode which will handle I/O requests.
  // |protocol| is the (potentially negotiated) vnode protocol that will be used to interact with
  //            the vnode over this connection.
  // |options| are client-specified options for this connection, converted from the flags and
  //           rights passed during the |fuchsia.io/Directory.Open| or |fuchsia.io/Node.Clone| FIDL
  //           call.
  Connection(fs::FuchsiaVfs* vfs, fbl::RefPtr<fs::Vnode> vnode, VnodeProtocol protocol,
             VnodeConnectionOptions options);

  VnodeProtocol protocol() const { return protocol_; }

  const VnodeConnectionOptions& options() const { return options_; }

  void set_append(bool append) { options_.flags.append = append; }

  FuchsiaVfs* vfs() const { return vfs_; }

  zx::event& token() { return token_; }

  // Flags which can be modified by SetFlags.
  constexpr static fuchsia_io::wire::OpenFlags kSettableStatusFlags =
      fuchsia_io::wire::OpenFlags::kAppend;

  // All flags which indicate state of the connection (excluding rights).
  constexpr static fuchsia_io::wire::OpenFlags kStatusFlags =
      kSettableStatusFlags | fuchsia_io::wire::OpenFlags::kNodeReference;

  // Node operations. Note that these provide the shared implementation of |fuchsia.io/Node|
  // methods, used by all connection subclasses.
  //
  // To simplify ownership handling, prefer using the |Vnode_*| types in return values, while using
  // the generated FIDL types in method arguments. This is because return values must recursively
  // own any child objects and handles to avoid a dangling reference.

  void NodeClone(fuchsia_io::wire::OpenFlags flags, fidl::ServerEnd<fuchsia_io::Node> server_end);
  zx::result<> NodeClose();
  fidl::VectorView<uint8_t> NodeQuery();
  virtual zx::result<VnodeRepresentation> NodeDescribe();
  void NodeSync(fit::callback<void(zx_status_t)> callback);
  zx::result<VnodeAttributes> NodeGetAttr();
  zx::result<> NodeSetAttr(fuchsia_io::wire::NodeAttributeFlags flags,
                           const fuchsia_io::wire::NodeAttributes& attributes);
  zx::result<fuchsia_io::wire::OpenFlags> NodeGetFlags();
  zx::result<> NodeSetFlags(fuchsia_io::wire::OpenFlags flags);
  zx::result<fuchsia_io::wire::FilesystemInfo> NodeQueryFilesystem();

 private:
  // The contract of the Vnode API is that there should be a balancing |Close| call for every |Open|
  // call made on a vnode. Calls |Close| on the underlying vnode explicitly if necessary.
  zx_status_t EnsureVnodeClosed();

  bool vnode_is_open_;

  // The Vfs instance which owns this connection. Connections must not outlive the Vfs, hence this
  // borrowing is safe.
  fs::FuchsiaVfs* const vfs_;

  fbl::RefPtr<fs::Vnode> vnode_;

  // State related to FIDL message dispatching. See |Binding|.
  std::unique_ptr<Binding> binding_;

  // The operational protocol that is used to interact with the vnode over this connection. It
  // provides finer grained information than the FIDL protocol, e.g. both a regular file and a
  // vmo-file could speak |fuchsia.io/File|.
  VnodeProtocol protocol_;

  // Client-specified connection options containing flags and rights passed during the
  // |fuchsia.io/Directory.Open| or |fuchsia.io/Node.Clone| FIDL call. Permissions on the underlying
  // Vnode are granted on a per-connection basis, and accessible from |options_.rights|.
  // Importantly, rights are hierarchical over Open/Clone. It is never allowed to derive a
  // Connection with more rights than the originating connection.
  VnodeConnectionOptions options_;

  // Handle to event which allows client to refer to open vnodes in multi-path operations (see:
  // link, rename). Defaults to ZX_HANDLE_INVALID. Validated on the server-side using cookies.
  zx::event token_ = {};
};

class Binding {
 public:
  virtual ~Binding() = default;

  virtual void Unbind() = 0;
  virtual void Close(zx_status_t) = 0;
};

template <typename Protocol>
class TypedBinding : public Binding {
 public:
  explicit TypedBinding(fidl::ServerBindingRef<Protocol> binding) : binding(binding) {}
  ~TypedBinding() override { binding.Unbind(); }

 private:
  void Unbind() override { return binding.Unbind(); }
  void Close(zx_status_t epitaph) override { return binding.Close(epitaph); }

  fidl::ServerBindingRef<Protocol> binding;
};

}  // namespace internal

}  // namespace fs

#endif  // SRC_STORAGE_LIB_VFS_CPP_CONNECTION_H_
