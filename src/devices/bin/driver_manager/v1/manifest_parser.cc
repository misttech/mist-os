// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v1/manifest_parser.h"

#include <fidl/fuchsia.component.decl/cpp/fidl.h>
#include <lib/fidl/cpp/natural_types.h>
#include <lib/zx/result.h>

#include "src/lib/fxl/strings/substitute.h"
#include "src/lib/pkg_url/fuchsia_pkg_url.h"

namespace {

const std::string kFuchsiaPkgPrefix = "fuchsia-pkg://";
const std::string kFuchsiaBootPrefix = "fuchsia-boot://";
const std::string kRelativeUrlPrefix = "#";

bool IsRelativeUrl(std::string_view url) {
  return url.compare(0, kRelativeUrlPrefix.length(), kRelativeUrlPrefix) == 0;
}

}  // namespace

bool IsFuchsiaBootScheme(std::string_view url) {
  return url.compare(0, kFuchsiaBootPrefix.length(), kFuchsiaBootPrefix) == 0;
}

zx::result<std::string> GetResourcePath(std::string_view url) {
  size_t seperator = url.find('#');
  if (seperator == std::string::npos) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  return zx::ok(url.substr(seperator + 1));
}

zx::result<std::string> GetBasePathFromUrl(const std::string& url) {
  if (IsFuchsiaBootScheme(url)) {
    auto resource_path = GetResourcePath(url);
    if (resource_path.is_error()) {
      LOGF(ERROR, "Failed to parse boot url: %s", url.c_str());
      return resource_path;
    }
    return zx::ok("/boot");
  }
  if (IsRelativeUrl(url)) {
    auto resource_path = GetResourcePath(url);
    if (resource_path.is_error()) {
      LOGF(ERROR, "Failed to parse test url: %s", url.c_str());
      return resource_path;
    }
    return zx::ok("/pkg");
  }

  return zx::error(ZX_ERR_NOT_FOUND);
}

zx::result<ManifestContent> ParseComponentManifest(zx::vmo vmo) {
  uint64_t size;
  zx::result result = zx::make_result(vmo.get_prop_content_size(&size));
  if (result.is_error()) {
    return result.take_error();
  }

  std::vector<uint8_t> buffer(size);
  result = zx::make_result(vmo.read(buffer.data(), 0, buffer.size()));
  if (result.is_error()) {
    return result.take_error();
  }

  fit::result component_decl = fidl::Unpersist<fuchsia_component_decl::Component>(buffer);
  if (component_decl.is_error()) {
    LOGF(ERROR, "Failed to unpersist: %s",
         component_decl.error_value().FormatDescription().c_str());
    return zx::error(component_decl.error_value().status());
  }

  if (!component_decl->program().has_value()) {
    LOGF(ERROR, "manifest lacks a program section");
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  auto& program = component_decl->program().value();

  if (!program.info().has_value()) {
    LOGF(ERROR, "manifest lacks a program.info section");
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  auto& info = program.info().value();

  if (!info.entries().has_value()) {
    LOGF(ERROR, "manifest lacks a program.info.entries section");
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  auto& entries = info.entries().value();

  std::string driver_url;
  std::string scheduler_role;
  for (const auto& entry : entries) {
    if (entry.key() == "compat") {
      if (!entry.value()->str().has_value()) {
        LOGF(ERROR, "dictionary value for program.compat is not string");
        return zx::error(ZX_ERR_NOT_FOUND);
      }
      driver_url = std::string("#") + entry.value()->str().value();
    } else if (entry.key() == "default_dispatcher_scheduler_role") {
      if (!entry.value()->str().has_value()) {
        LOGF(ERROR, "dictionary value for program.default_dispatcher_scheduler_role is not string");
        return zx::error(ZX_ERR_NOT_FOUND);
      }
      scheduler_role = entry.value()->str().value();
    }
  }
  if (driver_url.empty()) {
    LOGF(ERROR, "manifest lacks a program.info.compat decl");
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  ManifestContent content{
      .driver_url = driver_url,
      .service_uses = {},
      .default_dispatcher_scheduler_role = scheduler_role,
  };

  if (!component_decl->uses().has_value()) {
    LOGF(DEBUG, "manifest lacks a uses section");
    return zx::ok(std::move(content));
  }
  auto& uses = component_decl->uses().value();

  for (const auto& use : uses) {
    if (use.service().has_value()) {
      if (!use.service()->target_path().has_value()) {
        LOGF(ERROR, "service use without target_path");
        return zx::error(ZX_ERR_NOT_FOUND);
      }
      content.service_uses.push_back(use.service()->target_path().value());
    }
  }

  return zx::ok(std::move(content));
}
