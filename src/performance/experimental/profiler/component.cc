// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "component.h"

#include <lib/component/incoming/cpp/protocol.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace/event.h>
#include <lib/zx/result.h>

#include <string>
#include <vector>

#include "component_watcher.h"
#include "sampler.h"
#include "src/lib/fxl/memory/weak_ptr.h"
#include "taskfinder.h"

namespace {
// Reads a manifest from a |ManifestBytesIterator| producing a vector of bytes.
zx::result<std::vector<uint8_t>> DrainManifestBytesIterator(
    fidl::ClientEnd<fuchsia_sys2::ManifestBytesIterator> iterator_client_end) {
  TRACE_DURATION("cpu_profiler", __PRETTY_FUNCTION__);
  fidl::SyncClient iterator(std::move(iterator_client_end));
  std::vector<uint8_t> result;

  while (true) {
    auto next_res = iterator->Next();
    if (next_res.is_error()) {
      return zx::error(next_res.error_value().status());
    }

    if (next_res->infos().empty()) {
      break;
    }

    result.insert(result.end(), next_res->infos().begin(), next_res->infos().end());
  }

  return zx::ok(std::move(result));
}

zx::result<fidl::Box<fuchsia_component_decl::Component>> GetResolvedDeclaration(
    const std::string& moniker) {
  zx::result<fidl::ClientEnd<fuchsia_sys2::RealmQuery>> client_end =
      component::Connect<fuchsia_sys2::RealmQuery>("/svc/fuchsia.sys2.RealmQuery.root");
  if (client_end.is_error()) {
    FX_LOGS(WARNING) << "Unable to connect to RealmQuery. Component interaction is disabled";
    return client_end.take_error();
  }
  fidl::SyncClient realm_query_client{std::move(*client_end)};
  fidl::Result<::fuchsia_sys2::RealmQuery::GetResolvedDeclaration> declaration =
      realm_query_client->GetResolvedDeclaration({moniker});

  if (declaration.is_error()) {
    return zx::error(ZX_ERR_BAD_PATH);
  }

  auto drain_res = DrainManifestBytesIterator(std::move(declaration->iterator()));
  if (drain_res.is_error()) {
    return fit::error(drain_res.error_value());
  }

  auto unpersist_res = fidl::InplaceUnpersist<fuchsia_component_decl::wire::Component>(
      cpp20::span<uint8_t>(drain_res->begin(), drain_res->size()));
  if (unpersist_res.is_error()) {
    return zx::error(unpersist_res.error_value().status());
  }
  return zx::ok(fidl::ToNatural(*unpersist_res));
}
zx::result<zx_koid_t> ReadElfJobId(const fidl::SyncClient<fuchsia_io::Directory>& directory) {
  TRACE_DURATION("cpu_profiler", __PRETTY_FUNCTION__);
  zx::result<fidl::Endpoints<fuchsia_io::File>> endpoints =
      fidl::CreateEndpoints<fuchsia_io::File>();
  if (endpoints.is_error()) {
    return endpoints.take_error();
  }
  fit::result<fidl::OneWayStatus> res =
      directory->Open({{.path = "elf/job_id",
                        .flags = fuchsia_io::kPermReadable,
                        .options = {},
                        .object = endpoints->server.TakeChannel()}});
  if (res.is_error()) {
    return zx::error(ZX_ERR_IO);
  }

  fidl::SyncClient<fuchsia_io::File> job_id_file{std::move(endpoints->client)};
  fidl::Result<fuchsia_io::File::Read> read_res =
      job_id_file->Read({{.count = fuchsia_io::kMaxTransferSize}});
  if (read_res.is_error()) {
    return zx::error(ZX_ERR_IO);
  }
  std::string job_id_str(reinterpret_cast<const char*>(read_res->data().data()),
                         read_res->data().size());

  char* end;
  zx_koid_t job_id = std::strtoull(job_id_str.c_str(), &end, 10);
  if (end != job_id_str.c_str() + job_id_str.size()) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  return zx::ok(job_id);
}
zx::result<zx_koid_t> MonikerToJobId(const std::string& moniker) {
  TRACE_DURATION("cpu_profiler", __PRETTY_FUNCTION__);
  zx::result<fidl::ClientEnd<fuchsia_sys2::RealmQuery>> client_end =
      component::Connect<fuchsia_sys2::RealmQuery>("/svc/fuchsia.sys2.RealmQuery.root");
  if (client_end.is_error()) {
    FX_LOGS(WARNING) << "Unable to connect to RealmQuery. Attaching by moniker isn't supported!";
    return client_end.take_error();
  }
  auto [directory_client_endpoint, directory_server] =
      fidl::Endpoints<fuchsia_io::Directory>::Create();
  fidl::SyncClient<fuchsia_io::Directory> directory_client{std::move(directory_client_endpoint)};
  fidl::SyncClient realm_query_client{std::move(*client_end)};

  fidl::Result<fuchsia_sys2::RealmQuery::OpenDirectory> open_result =
      realm_query_client->OpenDirectory({{
          .moniker = moniker,
          .dir_type = fuchsia_sys2::OpenDirType::kRuntimeDir,
          .object = std::move(directory_server),
      }});
  if (open_result.is_error()) {
    FX_LOGS(WARNING) << "Unable to open the runtime directory of " << moniker << ": "
                     << open_result.error_value();
    return zx::error(ZX_ERR_BAD_PATH);
  }
  zx::result<zx_koid_t> job_id = ReadElfJobId(directory_client);
  if (job_id.is_error()) {
    FX_LOGS(WARNING) << "Unable to read component directory";
  }
  return job_id;
}
}  // namespace

profiler::ComponentWatcher::ComponentEventHandler profiler::MakeOnStartHandler(
    fxl::WeakPtr<profiler::Sampler> sampler) {
  return [sampler = std::move(sampler)](std::string moniker, std::string) {
    if (!sampler) {
      return;
    }
    elf_search::Searcher searcher;
    FX_LOGS(INFO) << "Attaching via moniker: " << moniker;
    zx::result<zx_koid_t> job_id = MonikerToJobId(moniker);
    if (job_id.is_error()) {
      FX_PLOGS(ERROR, job_id.error_value()) << "Failed to get Job ID from moniker";
      return;
    }
    TaskFinder tf;
    tf.AddJob(*job_id);
    zx::result<TaskFinder::FoundTasks> handles = tf.FindHandles();
    if (handles.is_error()) {
      FX_PLOGS(ERROR, handles.error_value()) << "Failed to find handle for: " << moniker;
      return;
    }
    for (auto& [koid, handle] : handles->jobs) {
      if (koid == job_id) {
        zx::result<profiler::JobTarget> target =
            profiler::MakeJobTarget(zx::job(handle.release()), searcher);
        if (target.is_error()) {
          FX_PLOGS(ERROR, target.status_value()) << "Failed to make target for: " << moniker;
          return;
        }
        zx::result<> target_result = sampler->AddTarget(std::move(*target));
        if (target_result.is_error()) {
          FX_PLOGS(ERROR, target_result.error_value()) << "Failed to add target for: " << moniker;
          return;
        }
        break;
      }
    }
  };
}

zx::result<> profiler::TraverseRealm(const std::string& moniker,
                                     const fit::function<zx::result<>(const std::string&)>& f) {
  TRACE_DURATION("cpu_profiler", __PRETTY_FUNCTION__, "moniker", moniker);
  if (zx::result self_res = f(moniker); self_res.is_error()) {
    return self_res;
  }
  auto manifest = GetResolvedDeclaration(moniker);
  if (manifest.is_error()) {
    // If this instance isn't launched yet, that's okay. We'll register to be notified when it does
    // launch. Skip it for now.
    return zx::ok();
  }

  if (manifest->children()) {
    for (const auto& child : *manifest->children()) {
      if (!child.name()) {
        return zx::error(ZX_ERR_BAD_PATH);
      }
      std::string child_moniker = moniker + "/" + *child.name();
      if (zx::result descendents_res = TraverseRealm(child_moniker, f);
          descendents_res.is_error()) {
        return descendents_res;
      }
    }
  }
  return zx::ok();
}

zx::result<profiler::Moniker> profiler::Moniker::Parse(std::string_view moniker) {
  // A valid moniker for launching in a dynamic collection looks like:
  //
  // parent_moniker/collection:name
  //
  // Where the parent_moniker and collection can be optional.

  std::optional<std::string> parent;
  if (size_t leaf_divider = moniker.find_last_of('/'); leaf_divider != std::string::npos) {
    parent = moniker.substr(0, leaf_divider);
    moniker = moniker.substr(leaf_divider + 1);
  }

  std::optional<std::string> collection;
  if (size_t collection_divider = moniker.find_last_of(':');
      collection_divider != std::string::npos) {
    collection = moniker.substr(0, collection_divider);
    moniker = moniker.substr(collection_divider + 1);
  }

  return zx::ok(Moniker{parent, collection, std::string{moniker}});
}

zx::result<std::unique_ptr<profiler::ControlledComponent>> profiler::ControlledComponent::Create(
    const std::string& url, const std::string& moniker_string,
    ComponentWatcher& component_watcher) {
  TRACE_DURATION("cpu_profiler", __PRETTY_FUNCTION__, "moniker", moniker_string, "url", url);
  zx::result moniker = profiler::Moniker::Parse(moniker_string);
  if (moniker.is_error()) {
    return moniker.take_error();
  }
  if (!moniker->collection.has_value()) {
    FX_LOGS(ERROR) << "Failed to create a component at moniker '" << moniker_string
                   << "'. Moniker is missing a collection";
    return zx::error(ZX_ERR_BAD_PATH);
  }
  std::unique_ptr component =
      std::make_unique<ControlledComponent>(url, *moniker, component_watcher);
  auto client_end = component::Connect<fuchsia_sys2::LifecycleController>(
      "/svc/fuchsia.sys2.LifecycleController.root");
  if (client_end.is_error()) {
    return client_end.take_error();
  }

  component->lifecycle_controller_client_ = fidl::SyncClient{std::move(*client_end)};

  fidl::Result<fuchsia_sys2::LifecycleController::CreateInstance> create_res =
      component->lifecycle_controller_client_->CreateInstance({{
          .parent_moniker = moniker->parent.value_or("."),
          .collection = *moniker->collection,
          .decl = {{
              .name = moniker->name,
              .url = url,
              .startup = fuchsia_component_decl::StartupMode::kLazy,
          }},
      }});

  if (create_res.is_error()) {
    FX_LOGS(ERROR) << "Failed to create  " << moniker_string << ": " << create_res.error_value();
    return zx::error(ZX_ERR_BAD_STATE);
  }

  fidl::Result<fuchsia_sys2::LifecycleController::ResolveInstance> resolve_res =
      component->lifecycle_controller_client_->ResolveInstance({{moniker->ToString()}});

  if (resolve_res.is_error()) {
    FX_LOGS(ERROR) << "Failed to resolve " << moniker_string << ": " << resolve_res.error_value();
    return zx::error(ZX_ERR_BAD_STATE);
  }

  return zx::ok(std::move(component));
}

zx::result<> profiler::ControlledComponent::Start(fxl::WeakPtr<Sampler> notify) {
  TRACE_DURATION("cpu_profiler", __PRETTY_FUNCTION__, "moniker", moniker_.ToString());
  on_start_ = profiler::MakeOnStartHandler(std::move(notify));
  zx::result<> watch_result = TraverseRealm(moniker_.ToString(), [this](std::string moniker) {
    return component_watcher_.WatchForMoniker(
        std::move(moniker), [this](std::string moniker, std::string url) {
          on_start_.value()(std::move(moniker), std::move(url));
        });
  });

  if (watch_result.is_error()) {
    return watch_result;
  }

  auto [binder_client, binder_server] = fidl::Endpoints<fuchsia_component::Binder>::Create();
  fidl::Result<fuchsia_sys2::LifecycleController::StartInstance> start_res =
      lifecycle_controller_client_->StartInstance({{
          .moniker = moniker_.ToString(),
          .binder = std::move(binder_server),
      }});

  if (start_res.is_error()) {
    FX_LOGS(ERROR) << "Failed to start component: " << start_res.error_value();
    return zx::error(ZX_ERR_UNAVAILABLE);
  }
  return zx::ok();
}

zx::result<> profiler::ControlledComponent::Stop() {
  TRACE_DURATION("cpu_profiler", __PRETTY_FUNCTION__, "moniker", moniker_.ToString());
  if (auto stop_res = lifecycle_controller_client_->StopInstance({{
          .moniker = moniker_.ToString(),
      }});
      stop_res.is_error()) {
    return zx::error(ZX_ERR_BAD_STATE);
  }
  return zx::ok();
}

zx::result<> profiler::ControlledComponent::Destroy() {
  TRACE_DURATION("cpu_profiler", __PRETTY_FUNCTION__, "moniker", moniker_.ToString());
  if (auto destroy_res = lifecycle_controller_client_->DestroyInstance({{
          .parent_moniker = moniker_.parent.value_or("."),
          .child = {{.name = moniker_.name, .collection = moniker_.collection}},
      }});
      destroy_res.is_error()) {
    FX_LOGS(ERROR) << "Failed to destroy " << moniker_.ToString() << ": "
                   << destroy_res.error_value();
    return zx::error(ZX_ERR_BAD_STATE);
  }
  needs_destruction_ = false;
  return zx::ok();
}

profiler::ControlledComponent::~ControlledComponent() {
  if (needs_destruction_) {
    (void)Destroy();
  }
}
