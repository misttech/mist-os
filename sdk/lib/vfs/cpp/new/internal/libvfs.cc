// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/vfs/cpp/new/internal/libvfs_private.h>
#include <lib/zx/channel.h>
#include <lib/zx/vmo.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>

#include <memory>
#include <type_traits>
#include <variant>

#include <fbl/ref_ptr.h>
#include <src/storage/lib/vfs/cpp/lazy_dir.h>
#include <src/storage/lib/vfs/cpp/pseudo_dir.h>
#include <src/storage/lib/vfs/cpp/pseudo_file.h>
#include <src/storage/lib/vfs/cpp/remote_dir.h>
#include <src/storage/lib/vfs/cpp/service.h>
#include <src/storage/lib/vfs/cpp/synchronous_vfs.h>
#include <src/storage/lib/vfs/cpp/vmo_file.h>
#include <src/storage/lib/vfs/cpp/vnode.h>

// NOLINTBEGIN(modernize-use-using): This library exposes a C interface.

namespace {

// Scope-based deleter for user-provided cookies.
using CookieDestroyer = std::unique_ptr<void, vfs_internal_destroy_cookie_t>;

CookieDestroyer MakeDestroyer(void* cookie, vfs_internal_destroy_cookie_t destroy) {
  if (!destroy) {
    return CookieDestroyer(nullptr, nullptr);
  }
  // If `cookie` is `nullptr`, `destroy` will not be invoked.
  return CookieDestroyer(cookie, destroy);
}

// Ensure constants defined in the SDK match those from the in-tree VFS.

using DefaultSharingMode = fs::VmoFile::DefaultSharingMode;

static_assert(
    std::is_same_v<vfs_internal_sharing_mode_t, std::underlying_type_t<DefaultSharingMode>>);
static_assert(static_cast<vfs_internal_sharing_mode_t>(DefaultSharingMode::kNone) ==
              VFS_INTERNAL_SHARING_MODE_NONE);

static_assert(static_cast<vfs_internal_sharing_mode_t>(DefaultSharingMode::kDuplicate) ==
              VFS_INTERNAL_SHARING_MODE_DUPLICATE);

static_assert(static_cast<vfs_internal_sharing_mode_t>(DefaultSharingMode::kCloneCow) ==
              VFS_INTERNAL_SHARING_MODE_COW);

// Implements `vfs::ComposedServiceDir` functionality using the in-tree VFS.
// TODO(https://fxbug.dev/309685624): Remove when all callers have migrated.
class LibvfsComposedServiceDir final : public fs::PseudoDir {
 public:
  zx_status_t Lookup(std::string_view name, fbl::RefPtr<fs::Vnode>* out) override {
    zx_status_t status = fs::PseudoDir::Lookup(name, out);
    if (status == ZX_OK) {
      return status;
    }
    if (fallback_dir_) {
      auto entry = fallback_services_.find(name);
      if (entry != fallback_services_.end()) {
        *out = entry->second;
      } else {
        auto connector = [name = std::string(name.data(), name.length()),
                          dir = &fallback_dir_](zx::channel channel) -> zx_status_t {
          auto response = fidl::WireCall(*dir)->Open(
              fuchsia_io::OpenFlags(), fuchsia_io::ModeType(), fidl::StringView::FromExternal(name),
              fidl::ServerEnd<fuchsia_io::Node>{std::move(channel)});
          if (!response.ok()) {
            return response.error().status();
          }
          return ZX_OK;
        };

        auto service = fbl::MakeRefCounted<fs::Service>(std::move(connector));
        *out = service;
        fallback_services_[std::string(name)] = std::move(service);
      }
      return ZX_OK;
    }
    return ZX_ERR_NOT_FOUND;
  }

  void SetFallback(fidl::ClientEnd<fuchsia_io::Directory> dir) { fallback_dir_ = std::move(dir); }

  zx_status_t AddService(std::string_view name, fbl::RefPtr<fs::Service> service) {
    return this->AddEntry(name, std::move(service));
  }

 private:
  friend fbl::internal::MakeRefCountedHelper<LibvfsComposedServiceDir>;
  friend fbl::RefPtr<LibvfsComposedServiceDir>;

  fidl::ClientEnd<fuchsia_io::Directory> fallback_dir_;

  // The collection of services that have been looked up on the fallback directory. These services
  // just forward connection requests to the fallback directory.
  mutable std::map<std::string, fbl::RefPtr<fs::Service>, std::less<>> fallback_services_;
};

// Implements in-tree `fs::LazyDir` using C callbacks defined in `vfs_internal_lazy_dir_context_t`.
// TODO(https://fxbug.dev/309685624): Remove when all callers have migrated.

class LibvfsLazyDir final : public fs::LazyDir {
 public:
  explicit LibvfsLazyDir(const vfs_internal_lazy_dir_context_t* context) : context_(*context) {}

 protected:
  friend fbl::internal::MakeRefCountedHelper<LibvfsLazyDir>;
  friend fbl::RefPtr<LibvfsLazyDir>;

  void GetContents(fbl::Vector<LazyEntry>* out_vector) override;
  zx_status_t GetFile(fbl::RefPtr<Vnode>* out_vnode, uint64_t id, fbl::String name) override;

 private:
  vfs_internal_lazy_dir_context_t context_;
};

}  // namespace

typedef struct vfs_internal_vfs {
  std::unique_ptr<fs::FuchsiaVfs> vfs;
} vfs_internal_vfs_t;

typedef struct vfs_internal_node {
  using NodeVariant =
      std::variant<fbl::RefPtr<fs::PseudoDir>, fbl::RefPtr<fs::Service>, fbl::RefPtr<fs::RemoteDir>,
                   fbl::RefPtr<fs::VmoFile>, fbl::RefPtr<fs::BufferedPseudoFile>,
                   fbl::RefPtr<LibvfsComposedServiceDir>, fbl::RefPtr<LibvfsLazyDir>>;
  NodeVariant node;

  template <typename T>
  T* Downcast() {
    if (auto* ptr = std::get_if<fbl::RefPtr<T>>(&node)) {
      return ptr->get();
    }
    return nullptr;
  }

  fbl::RefPtr<fs::Vnode> AsNode() const {
    return std::visit([](const auto& node) { return fbl::RefPtr<fs::Vnode>(node); }, node);
  }
} vfs_internal_node_t;

__EXPORT zx_status_t vfs_internal_create(async_dispatcher_t* dispatcher,
                                         vfs_internal_vfs_t** out_vfs) {
  if (!out_vfs || !dispatcher) {
    return ZX_ERR_INVALID_ARGS;
  }
  *out_vfs = new vfs_internal_vfs_t{.vfs = std::make_unique<fs::SynchronousVfs>(dispatcher)};
  return ZX_OK;
}

__EXPORT zx_status_t vfs_internal_destroy(vfs_internal_vfs_t* vfs) {
  if (!vfs) {
    return ZX_ERR_INVALID_ARGS;
  }
  delete vfs;
  return ZX_OK;
}

__EXPORT zx_status_t vfs_internal_serve(vfs_internal_vfs_t* vfs, const vfs_internal_node_t* vnode,
                                        zx_handle_t channel, uint32_t rights) {
  zx::channel chan(channel);
  if (!vfs || !vnode) {
    return ZX_ERR_INVALID_ARGS;
  }
  if (!chan) {
    return ZX_ERR_BAD_HANDLE;
  }
  // Ensure `rights` are compatible with the version this library was compiled against.
  std::optional fidl_rights = fuchsia_io::wire::OpenFlags::TryFrom(rights);
  if (!fidl_rights) {
    return ZX_ERR_INVALID_ARGS;
  }
  return vfs->vfs->Serve(vnode->AsNode(), std::move(chan),
                         fs::VnodeConnectionOptions::FromIoV1Flags(*fidl_rights));
}

__EXPORT zx_status_t vfs_internal_node_destroy(vfs_internal_node_t* vnode) {
  if (!vnode) {
    return ZX_ERR_INVALID_ARGS;
  }
  delete vnode;
  return ZX_OK;
}

__EXPORT zx_status_t vfs_internal_directory_create(vfs_internal_node_t** out_vnode) {
  if (!out_vnode) {
    return ZX_ERR_INVALID_ARGS;
  }
  *out_vnode = new vfs_internal_node_t{.node = fbl::MakeRefCounted<fs::PseudoDir>()};
  return ZX_OK;
}

__EXPORT zx_status_t vfs_internal_directory_add(vfs_internal_node_t* dir,
                                                const vfs_internal_node_t* vnode,
                                                const char* name) {
  if (!dir || !vnode || !name) {
    return ZX_ERR_INVALID_ARGS;
  }
  fs::PseudoDir* downcasted = dir->Downcast<fs::PseudoDir>();
  if (!downcasted) {
    return ZX_ERR_NOT_DIR;
  }
  return downcasted->AddEntry(name, vnode->AsNode());
}

__EXPORT zx_status_t vfs_internal_directory_remove(vfs_internal_node_t* dir, const char* name) {
  if (!dir || !name) {
    return ZX_ERR_INVALID_ARGS;
  }
  fs::PseudoDir* downcasted = dir->Downcast<fs::PseudoDir>();
  if (!downcasted) {
    return ZX_ERR_WRONG_TYPE;
  }
  return downcasted->RemoveEntry(name);
}

__EXPORT zx_status_t vfs_internal_remote_directory_create(zx_handle_t remote,
                                                          vfs_internal_node_t** out_vnode) {
  if (!out_vnode) {
    return ZX_ERR_INVALID_ARGS;
  }
  // TODO(https://fxbug.dev/293936429): We might have to relax this check, as RemoteDir should
  // gracefully handle this case. The existing SDK VFS node constructors are infallible even when
  // `remote` is invalid.
  if (remote == ZX_HANDLE_INVALID) {
    return ZX_ERR_BAD_HANDLE;
  }
  *out_vnode =
      new vfs_internal_node_t{.node = fbl::MakeRefCounted<fs::RemoteDir>(
                                  fidl::ClientEnd<fuchsia_io::Directory>{zx::channel(remote)})};
  return ZX_OK;
}

__EXPORT zx_status_t vfs_internal_service_create(const vfs_internal_svc_context_t* context,
                                                 vfs_internal_node_t** out_vnode) {
  // When the last reference to this node is dropped we must ensure the context cookie is destroyed.
  // We do this by capturing a destroyer inside the service connector, which is owned by the node.
  CookieDestroyer destroyer = MakeDestroyer(context->cookie, context->destroy);
  if (!context || !context->connect) {
    return ZX_ERR_INVALID_ARGS;
  }
  fs::Service::Connector connector = [context = *context,
                                      destroyer = std::move(destroyer)](zx::channel channel) {
    return context.connect(context.cookie, channel.release());
  };
  *out_vnode =
      new vfs_internal_node_t{.node = fbl::MakeRefCounted<fs::Service>(std::move(connector))};
  return ZX_OK;
}

__EXPORT zx_status_t vfs_internal_vmo_file_create(zx_handle_t vmo_handle, uint64_t length,
                                                  vfs_internal_write_mode_t writable,
                                                  vfs_internal_sharing_mode_t sharing_mode,
                                                  vfs_internal_node_t** out_vnode) {
  zx::vmo vmo(vmo_handle);
  if (!out_vnode) {
    return ZX_ERR_INVALID_ARGS;
  }
  // TODO(https://fxbug.dev/293936429): We might have to relax this check, as VmoFile should
  // gracefully handle this case. The existing SDK VFS node constructors are infallible even when
  // `vmo` is invalid.
  if (vmo == ZX_HANDLE_INVALID) {
    return ZX_ERR_BAD_HANDLE;
  }

  // We statically verify the sharing mode constants are the same between both libraries above.
  *out_vnode = new vfs_internal_node_t{
      .node = fbl::MakeRefCounted<fs::VmoFile>(std::move(vmo), static_cast<size_t>(length),
                                               writable == VFS_INTERNAL_WRITE_MODE_WRITABLE,
                                               static_cast<DefaultSharingMode>(sharing_mode))};
  return ZX_OK;
}

__EXPORT zx_status_t vfs_internal_pseudo_file_create(size_t max_bytes,
                                                     const vfs_internal_file_context_t* context,
                                                     vfs_internal_node_t** out_vnode) {
  // When the last reference to this node is dropped we must ensure the context cookie is destroyed.
  // We do this by capturing a destroyer inside the read handler, which is owned by the node.
  // *NOTE*: The cookie is shared across both the read and write callbacks, but we only capture the
  // destroyer in the read handler. This is because the read handler is declared first in the
  // pseudo-file, so it is destroyed last.
  CookieDestroyer destroyer = MakeDestroyer(context->cookie, context->destroy);
  if (!context || !context->read) {
    return ZX_ERR_INVALID_ARGS;
  }
  fs::BufferedPseudoFile::ReadHandler read_handler =
      [context = *context, destroyer = std::move(destroyer)](fbl::String* output) -> zx_status_t {
    const char* data;
    size_t len;
    if (zx_status_t status = context.read(context.cookie, &data, &len); status != ZX_OK) {
      return status;
    }
    *output = fbl::String(data, len);
    if (context.release) {
      context.release(context.cookie);
    }
    return ZX_OK;
  };
  fs::BufferedPseudoFile::WriteHandler write_handler = nullptr;
  if (context->write) {
    write_handler = [context = *context](std::string_view input) -> zx_status_t {
      return context.write(context.cookie, input.data(), input.length());
    };
  }

  *out_vnode =
      new vfs_internal_node_t{.node = fbl::MakeRefCounted<fs::BufferedPseudoFile>(
                                  std::move(read_handler), std::move(write_handler), max_bytes)};
  return ZX_OK;
}

__EXPORT zx_status_t vfs_internal_composed_svc_dir_create(vfs_internal_node_t** out_vnode) {
  if (!out_vnode) {
    return ZX_ERR_INVALID_ARGS;
  }
  *out_vnode = new vfs_internal_node_t{.node = fbl::MakeRefCounted<LibvfsComposedServiceDir>()};
  return ZX_OK;
}

__EXPORT zx_status_t vfs_internal_composed_svc_dir_add(vfs_internal_node_t* dir,
                                                       const vfs_internal_node_t* service_node,
                                                       const char* name) {
  if (!dir || !service_node || !name) {
    return ZX_ERR_INVALID_ARGS;
  }
  LibvfsComposedServiceDir* downcasted = dir->Downcast<LibvfsComposedServiceDir>();
  if (!downcasted) {
    return ZX_ERR_WRONG_TYPE;
  }
  return downcasted->AddEntry(name, service_node->AsNode());
}

__EXPORT zx_status_t vfs_internal_composed_svc_dir_set_fallback(vfs_internal_node_t* dir,
                                                                zx_handle_t fallback_channel) {
  if (!dir) {
    return ZX_ERR_INVALID_ARGS;
  }
  LibvfsComposedServiceDir* downcasted = dir->Downcast<LibvfsComposedServiceDir>();
  if (!downcasted) {
    return ZX_ERR_WRONG_TYPE;
  }
  if (fallback_channel == ZX_HANDLE_INVALID) {
    return ZX_ERR_BAD_HANDLE;
  }
  downcasted->SetFallback(fidl::ClientEnd<fuchsia_io::Directory>{zx::channel(fallback_channel)});
  return ZX_OK;
}

namespace {

void LibvfsLazyDir::GetContents(fbl::Vector<LazyEntry>* out_vector) {
  vfs_internal_lazy_entry_t* entries;
  size_t num_entries;
  context_.get_contents(context_.cookie, &entries, &num_entries);
  out_vector->reset();
  out_vector->reserve(num_entries);
  for (size_t i = 0; i < num_entries; ++i) {
    out_vector->push_back(LazyEntry{
        .id = entries[i].id,
        .name = entries[i].name,
        .type = entries[i].type,
    });
  }
}

zx_status_t LibvfsLazyDir::GetFile(fbl::RefPtr<Vnode>* out_vnode, uint64_t id, fbl::String name) {
  vfs_internal_node_t* node;
  if (zx_status_t status = context_.get_entry(context_.cookie, &node, id, name.data());
      status != ZX_OK) {
    return status;
  }
  *out_vnode = node->AsNode();
  return ZX_OK;
}

}  // namespace

__EXPORT zx_status_t vfs_internal_lazy_dir_create(const vfs_internal_lazy_dir_context* context,
                                                  vfs_internal_node_t** out_vnode) {
  if (!context || !out_vnode || !context->cookie || !context->get_contents || !context->get_entry) {
    return ZX_ERR_INVALID_ARGS;
  }
  *out_vnode = new vfs_internal_node_t{.node = fbl::MakeRefCounted<LibvfsLazyDir>(context)};
  return ZX_OK;
}

// NOLINTEND(modernize-use-using)
