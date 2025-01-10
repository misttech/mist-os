// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/runner.h"

#include <lib/syslog/cpp/macros.h>
#include <zircon/processargs.h>

#include "src/devices/lib/log/log.h"

namespace {

namespace fdi = fuchsia_driver_index;
namespace fio = fuchsia_io;
namespace fprocess = fuchsia_process;
namespace frunner = fuchsia_component_runner;
namespace fcomponent = fuchsia_component;
namespace fdecl = fuchsia_component_decl;

constexpr uint32_t kTokenId = PA_HND(PA_USER0, 0);

zx::result<zx_koid_t> GetKoid(zx_handle_t handle) {
  zx_info_handle_basic_t info{};
  if (zx_status_t status =
          zx_object_get_info(handle, ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
      status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(info.koid);
}

const char* GetErrorString(fcomponent::Error error) {
  switch (error) {
    case fcomponent::Error::kInternal:
      return "INTERNAL";
    case fcomponent::Error::kInvalidArguments:
      return "INVALID_ARGUMENTS";
    case fcomponent::Error::kUnsupported:
      return "UNSUPPORTED";
    case fcomponent::Error::kAccessDenied:
      return "ACCESS_DENIED";
    case fcomponent::Error::kInstanceNotFound:
      return "INSTANCE_NOT_FOUND";
    case fcomponent::Error::kInstanceAlreadyExists:
      return "INSTANCE_ALREADY_EXISTS";
    case fcomponent::Error::kInstanceCannotStart:
      return "INSTANCE_CANNOT_START";
    case fcomponent::Error::kInstanceCannotResolve:
      return "INSTANCE_CANNOT_RESOLVE";
    case fcomponent::Error::kCollectionNotFound:
      return "COLLECTION_NOT_FOUND";
    case fcomponent::Error::kResourceUnavailable:
      return "RESOURCE_UNAVAILABLE";
    case fcomponent::Error::kInstanceDied:
      return "INSTANCE_DIED";
    case fcomponent::Error::kResourceNotFound:
      return "RESOURCE_NOT_FOUND";
    case fcomponent::Error::kInstanceCannotUnresolve:
      return "INSTANCE_CANNOT_UNRESOLVE";
    case fcomponent::Error::kInstanceAlreadyStarted:
      return "INSTANCE_ALREADY_STARTED";
    default:
      return "UNKNOWN";
  }
}

}  // namespace

namespace driver_manager {

zx::result<> Runner::Publish(component::OutgoingDirectory& outgoing) {
  return outgoing.AddUnmanagedProtocol<frunner::ComponentRunner>(
      bindings_.CreateHandler(this, dispatcher_, fidl::kIgnoreBindingClosure));
}

void Runner::StartDriverComponent(
    std::string_view moniker, std::string_view url, std::string_view collection_name,
    const std::vector<NodeOffer>& offers,
    std::optional<fuchsia_component_sandbox::DictionaryRef> dictionary_ref,
    StartCallback callback) {
  zx::event token;
  zx_status_t status = zx::event::create(0, &token);
  if (status != ZX_OK) {
    return callback(zx::error(status));
  }

  zx::result koid = GetKoid(token.get());
  if (koid.is_error()) {
    return callback(koid.take_error());
  }
  start_requests_.emplace(koid.value(), std::move(callback));

  fidl::Arena arena;
  auto child_decl = fdecl::wire::Child::Builder(arena)
                        .name(fidl::StringView::FromExternal(moniker))
                        .url(fidl::StringView::FromExternal(url))
                        .startup(fdecl::wire::StartupMode::kLazy)
                        .Build();

  fprocess::wire::HandleInfo handle_info = {
      .handle = std::move(token),
      .id = kTokenId,
  };

  auto child_args_builder = fcomponent::wire::CreateChildArgs::Builder(arena).numbered_handles(
      fidl::VectorView<fprocess::wire::HandleInfo>::FromExternal(&handle_info, 1));

  size_t offers_count;
  if (!dictionary_ref.has_value()) {
    offers_count = offers.size() + offer_injector_.ExtraOffersCount();
  } else {
    offers_count = offers.size();
  }
  fidl::VectorView<fdecl::wire::Offer> dynamic_offers(arena, offers_count);
  if (!offers.empty()) {
    for (size_t i = 0; i < offers.size(); i++) {
      const auto& offer = offers[i];
      zx::result get_offer_result = GetInnerOffer(offer);
      if (get_offer_result.is_error()) {
        return callback(get_offer_result.take_error());
      }

      auto [inner_offer, _] = get_offer_result.value();
      dynamic_offers[i] = inner_offer;
    }
  }
  if (!dictionary_ref.has_value()) {
    offer_injector_.Inject(arena, dynamic_offers, offers.size());
  }

  child_args_builder.dynamic_offers(dynamic_offers);

  if (dictionary_ref.has_value()) {
    child_args_builder.dictionary(fidl::ToWire(arena, *std::move(dictionary_ref)));
  }

  auto create_callback =
      [this, child_moniker = std::string(moniker.data()), koid = koid.value()](
          fidl::WireUnownedResult<fcomponent::Realm::CreateChild>& result) mutable {
        bool is_error = false;
        if (!result.ok()) {
          LOGF(ERROR, "Failed to create child '%s': %s", child_moniker.c_str(),
               result.FormatDescription().c_str());
          is_error = true;
        } else if (result.value().is_error()) {
          LOGF(ERROR, "Failed to create child '%s': %s", child_moniker.c_str(),
               GetErrorString(result.value().error_value()));
          is_error = true;
        }
        if (is_error) {
          zx::result result = CallCallback(koid, zx::error(ZX_ERR_INTERNAL));
          if (result.is_error()) {
            LOGF(ERROR, "Failed to find driver request for '%s': %s", child_moniker.c_str(),
                 result.status_string());
          }
        }
      };
  realm_
      ->CreateChild(
          fdecl::wire::CollectionRef{
              .name = fidl::StringView::FromExternal(collection_name),
          },
          child_decl, child_args_builder.Build())
      .Then(std::move(create_callback));
}

void Runner::Start(StartRequestView request, StartCompleter::Sync& completer) {
  std::string url = std::string(request->start_info.resolved_url().get());

  // When we start a driver, we associate an unforgeable token (the KOID of a
  // zx::event) with the start request, through the use of the numbered_handles
  // field. We do this so:
  //  1. We can securely validate the origin of the request
  //  2. We avoid collisions that can occur when relying on the package URL
  //  3. We avoid relying on the resolved URL matching the package URL
  if (!request->start_info.has_numbered_handles()) {
    LOGF(ERROR, "Failed to start driver '%s', invalid request for driver", url.c_str());
    completer.Close(ZX_ERR_INVALID_ARGS);
    return;
  }
  auto& handles = request->start_info.numbered_handles();
  if (handles.count() != 1 || !handles[0].handle || handles[0].id != kTokenId) {
    LOGF(ERROR, "Failed to start driver '%s', invalid request for driver", url.c_str());
    completer.Close(ZX_ERR_INVALID_ARGS);
    return;
  }

  zx::result koid = GetKoid(handles[0].handle.get());
  if (koid.is_error()) {
    completer.Close(ZX_ERR_INVALID_ARGS);
    return;
  }

  zx::result result = CallCallback(koid.value(), zx::ok(StartedComponent{
                                                     .info = fidl::ToNatural(request->start_info),
                                                     .controller = std::move(request->controller),
                                                 }));
  if (result.is_error()) {
    LOGF(ERROR, "Failed to start driver '%s', unknown request for driver", url.c_str());
    completer.Close(ZX_ERR_UNAVAILABLE);
  }
}

void Runner::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_component_runner::ComponentRunner> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FX_LOG_KV(WARNING, "Unknown ComponentRunner request", FX_KV("ordinal", metadata.method_ordinal));
}

zx::result<> Runner::CallCallback(zx_koid_t koid, zx::result<StartedComponent> component) {
  auto it = start_requests_.find(koid);
  if (it == start_requests_.end()) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  auto callback = std::move(it->second);
  start_requests_.erase(koid);

  callback(std::move(component));
  return zx::ok();
}

}  // namespace driver_manager
