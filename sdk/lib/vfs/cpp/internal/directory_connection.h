// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_VFS_CPP_INTERNAL_DIRECTORY_CONNECTION_H_
#define LIB_VFS_CPP_INTERNAL_DIRECTORY_CONNECTION_H_

#include <fuchsia/io/cpp/fidl.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/vfs/cpp/internal/connection.h>

#include <memory>

namespace vfs {

namespace internal {
class Directory;

// Binds an implementation of |fuchsia.io.Directory| to a
// |vfs::internal::Directory|.
class DirectoryConnection final : public Connection, public fuchsia::io::Directory {
 public:
  // Create a connection to |vn| with the given |flags|.
  DirectoryConnection(fuchsia::io::OpenFlags flags, vfs::internal::Directory* vn);
  ~DirectoryConnection() override;

  // Start listening for |fuchsia.io.Directory| messages on |request|.
  zx_status_t BindInternal(zx::channel request, async_dispatcher_t* dispatcher) override;

  // |fuchsia::io::Directory| Implementation:
  void AdvisoryLock(fuchsia::io::AdvisoryLockRequest request,
                    AdvisoryLockCallback callback) override;
  void Clone(fuchsia::io::OpenFlags flags,
             fidl::InterfaceRequest<fuchsia::io::Node> object) override;
  void Close(CloseCallback callback) override;
  void Query(QueryCallback callback) override;
  void GetConnectionInfo(GetConnectionInfoCallback callback) override;
  void Sync(SyncCallback callback) override;
  void GetAttr(GetAttrCallback callback) override;
  void SetAttr(fuchsia::io::NodeAttributeFlags flags, fuchsia::io::NodeAttributes attributes,
               SetAttrCallback callback) override;
  void Open(fuchsia::io::OpenFlags flags, fuchsia::io::ModeType mode, std::string path,
            fidl::InterfaceRequest<fuchsia::io::Node> object) override;
  void Unlink(std::string name, fuchsia::io::UnlinkOptions options,
              UnlinkCallback callback) override;
  void ReadDirents(uint64_t max_bytes, ReadDirentsCallback callback) override;
  void Rewind(RewindCallback callback) override;
  void GetToken(GetTokenCallback callback) override;
  void Rename(std::string src, zx::event dst_parent_token, std::string dst,
              RenameCallback callback) override;
  void Link(std::string src, zx::handle dst_parent_token, std::string dst,
            LinkCallback callback) override;
  void Watch(fuchsia::io::WatchMask mask, uint32_t options,
             fidl::InterfaceRequest<fuchsia::io::DirectoryWatcher> watcher,
             WatchCallback callback) override;
  void GetFlags(GetFlagsCallback callback) override;
  void SetFlags(fuchsia::io::OpenFlags flags, SetFlagsCallback callback) override;
  void QueryFilesystem(QueryFilesystemCallback callback) override {
    callback(ZX_ERR_NOT_SUPPORTED, nullptr);
  }
  void Enumerate(fuchsia::io::DirectoryEnumerateOptions options,
                 fidl::InterfaceRequest<fuchsia::io::DirectoryIterator> iterator) override {
    iterator.Close(ZX_ERR_NOT_SUPPORTED);
  }
  void GetAttributes(fuchsia::io::NodeAttributesQuery query,
                     GetAttributesCallback callback) override {
    callback(fuchsia::io::Node2_GetAttributes_Result::WithErr(ZX_ERR_NOT_SUPPORTED));
  }
  void Reopen(fuchsia::io::RightsRequest rights_request,
              fidl::InterfaceRequest<fuchsia::io::Node> object_request) override {
    object_request.Close(ZX_ERR_NOT_SUPPORTED);
  }
  void UpdateAttributes(fuchsia::io::MutableNodeAttributes mutable_node_attributes,
                        UpdateAttributesCallback callback) override {
    callback(fuchsia::io::Node2_UpdateAttributes_Result::WithErr(ZX_ERR_NOT_SUPPORTED));
  }
  void Open2(std::string path, fuchsia::io::ConnectionProtocols protocols,
             zx::channel object_request) override {
    fidl::InterfaceRequest<fuchsia::io::Node>(std::move(object_request))
        .Close(ZX_ERR_NOT_SUPPORTED);
  }
#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
  void CreateSymlink(std::string name, std::vector<uint8_t> target,
                     fidl::InterfaceRequest<::fuchsia::io::Symlink> connection,
                     CreateSymlinkCallback callback) override {
    callback(fuchsia::io::Directory2_CreateSymlink_Result::WithErr(ZX_ERR_NOT_SUPPORTED));
  }
  void ListExtendedAttributes(
      fidl::InterfaceRequest<fuchsia::io::ExtendedAttributeIterator> iterator) override {
    iterator.Close(ZX_ERR_NOT_SUPPORTED);
  }
  void GetExtendedAttribute(std::vector<uint8_t> attribute,
                            GetExtendedAttributeCallback callback) override {
    callback(fuchsia::io::Node2_GetExtendedAttribute_Result::WithErr(ZX_ERR_NOT_SUPPORTED));
  }
  void SetExtendedAttribute(std::vector<uint8_t> attribute,
                            fuchsia::io::ExtendedAttributeValue value,
                            SetExtendedAttributeCallback callback) override {
    callback(fuchsia::io::Node2_SetExtendedAttribute_Result::WithErr(ZX_ERR_NOT_SUPPORTED));
  }
  void RemoveExtendedAttribute(std::vector<uint8_t> attribute,
                               RemoveExtendedAttributeCallback callback) override {
    callback(fuchsia::io::Node2_RemoveExtendedAttribute_Result::WithErr(ZX_ERR_NOT_SUPPORTED));
  }
#endif

 protected:
  // |Connection| Implementation:
  void SendOnOpenEvent(zx_status_t status) override;

 private:
  vfs::internal::Directory* vn_;
  fidl::Binding<fuchsia::io::Directory> binding_;
};

}  // namespace internal
}  // namespace vfs

#endif  // LIB_VFS_CPP_INTERNAL_DIRECTORY_CONNECTION_H_
