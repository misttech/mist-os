// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/composite_assembler.h"

#include <lib/driver/component/cpp/node_add_args.h>

#include "src/devices/lib/log/log.h"
#include "src/storage/lib/vfs/cpp/service.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}  //  namespace fdf

namespace fdd = fuchsia_driver_development;
namespace fdl = fuchsia_driver_legacy;
namespace fdm = fuchsia_device_manager;

namespace dfv2 {

namespace {

fbl::Array<const zx_device_prop_t> NodeToProps(Node* node) {
  std::vector<zx_device_prop_t> props;
  for (auto& prop : node->properties()) {
    if (prop.key.is_int_value() && prop.value.is_int_value()) {
      zx_device_prop_t device_prop;
      device_prop.id = static_cast<uint16_t>(prop.key.int_value());
      device_prop.value = prop.value.int_value();
      props.push_back(device_prop);
    }
  }
  fbl::Array<zx_device_prop_t> props_array(new zx_device_prop_t[props.size()], props.size());
  for (size_t i = 0; i < props.size(); i++) {
    props_array[i] = props[i];
  }
  return props_array;
}

}  // namespace

zx::result<CompositeDeviceFragment> CompositeDeviceFragment::Create(fdm::DeviceFragment fragment) {
  auto composite_fragment = CompositeDeviceFragment();

  if (fragment.parts().size() != 1) {
    LOGF(ERROR, "Composite fragments with multiple parts are deprecated. %s has %zd parts.",
         fragment.name().c_str(), fragment.parts().size());
    return zx::error(ZX_ERR_INTERNAL);
  }

  auto& program = fragment.parts()[0].match_program();
  std::vector<zx_bind_inst_t> rules(program.size());
  for (size_t i = 0; i < program.size(); i++) {
    rules[i] = zx_bind_inst_t{
        .op = program[i].op(),
        .arg = program[i].arg(),
    };
  }
  composite_fragment.name_ = fragment.name();
  composite_fragment.bind_rules_ = std::move(rules);

  return zx::ok(std::move(composite_fragment));
}

fdl::CompositeFragmentInfo CompositeDeviceFragment::GetCompositeFragmentInfo() const {
  fdl::CompositeFragmentInfo fragment_info;
  fragment_info.name() = name_;

  std::vector<fuchsia_driver_legacy::BindInstruction> bind_rules(bind_rules_.size());
  for (size_t i = 0; i < bind_rules_.size(); i++) {
    bind_rules[i] = fuchsia_driver_legacy::BindInstruction{{
        .op = bind_rules_[i].op,
        .arg = bind_rules_[i].arg,
        .debug = bind_rules_[i].debug,
    }};
  }
  fragment_info.bind_rules() = std::move(bind_rules);
  return fragment_info;
}

std::optional<std::string> CompositeDeviceFragment::GetTopologicalPath() const {
  if (auto node = bound_node_.lock()) {
    return node->MakeTopologicalPath();
  }

  return std::nullopt;
}

bool CompositeDeviceFragment::BindNode(std::shared_ptr<Node> node) {
  // If we have a bound node, then don't match.
  if (bound_node_.lock()) {
    return false;
  }

  internal::BindProgramContext context = {};
  context.binding = bind_rules_.data();
  context.binding_size = bind_rules_.size() * sizeof(zx_bind_inst_t);

  auto props = NodeToProps(node.get());
  context.props = &props;

  if (!EvaluateBindProgram(&context)) {
    return false;
  }

  // We matched! Store our node.
  bound_node_ = node;
  return true;
}

void CompositeDeviceFragment::Inspect(inspect::Node& root) const {
  std::string moniker = "<unbound>";
  if (auto node = bound_node_.lock()) {
    moniker = node->MakeComponentMoniker();
  }

  root.RecordString(name_, moniker);
}

fdd::CompositeNodeInfo CompositeDeviceAssembler::GetCompositeInfo() const {
  std::vector<std::optional<std::string>> parent_topological_paths;
  parent_topological_paths.reserve(fragments_.size());
  for (auto& fragment : fragments_) {
    parent_topological_paths.push_back(fragment.GetTopologicalPath());
  }

  std::optional<std::string> topological_path = std::nullopt;
  if (assembled_node_.has_value()) {
    if (auto node_ptr = assembled_node_->lock()) {
      topological_path = node_ptr->MakeTopologicalPath();
    }
  }

  auto info = fdd::CompositeNodeInfo({
      .parent_topological_paths = parent_topological_paths,
      .topological_path = topological_path,
      .composite = fdd::CompositeInfo::WithLegacyComposite(GetLegacyCompositeInfo()),
  });

  return info;
}

fuchsia_driver_legacy::CompositeInfo CompositeDeviceAssembler::GetLegacyCompositeInfo() const {
  std::vector<fdl::CompositeFragmentInfo> fragments;
  fragments.reserve(fragments_.size());
  for (auto& fragment : fragments_) {
    fragments.push_back(fragment.GetCompositeFragmentInfo());
  }

  std::vector<fdf::NodeProperty> properties;
  properties.reserve(properties_.size());
  for (auto& prop : properties_) {
    properties.push_back(fidl::ToNatural(prop));
  }

  std::optional<fdf::DriverInfo> driver_info = std::nullopt;
  if (assembled_node_.has_value()) {
    if (auto node_ptr = assembled_node_->lock()) {
      driver_info = fdf::DriverInfo({
          .url = node_ptr->driver_url(),
          .package_type = node_ptr->driver_package_type(),
      });
    }
  }

  fdl::CompositeInfo legacy_composite_info({
      .name = name_,
      .fragments = fragments,
      .properties = properties,
      .matched_driver = driver_info,
      .primary_fragment_index = 0,  // The first fragment is always the primary index.
  });

  return legacy_composite_info;
}

zx::result<std::unique_ptr<CompositeDeviceAssembler>> CompositeDeviceAssembler::Create(
    std::string name, fuchsia_device_manager::CompositeDeviceDescriptor descriptor,
    NodeManager* node_manager, async_dispatcher_t* dispatcher) {
  auto assembler = std::make_unique<CompositeDeviceAssembler>();
  assembler->name_ = std::move(name);
  assembler->node_manager_ = node_manager;
  assembler->dispatcher_ = dispatcher;

  if (descriptor.primary_fragment_index() >= descriptor.fragments().size()) {
    LOGF(ERROR,
         "Composite fragments with bad primary_fragment_index. primary is %ul but composite has "
         "%zd parts.",
         descriptor.primary_fragment_index(), descriptor.fragments().size());
    return zx::error(ZX_ERR_INTERNAL);
  }

  // Create the properties.
  for (auto& prop : descriptor.props()) {
    assembler->properties_.emplace_back(
        fdf::MakeProperty(assembler->arena_, prop.id(), prop.value()));
  }

  // Create the string properties.
  for (auto& prop : descriptor.str_props()) {
    switch (prop.value().Which()) {
      case fuchsia_driver_legacy::PropertyValue::Tag::kBoolValue:
        assembler->properties_.emplace_back(
            fdf::MakeProperty(assembler->arena_, prop.key(), prop.value().bool_value().value()));
        break;

      case fuchsia_driver_legacy::PropertyValue::Tag::kIntValue:
        assembler->properties_.emplace_back(
            fdf::MakeProperty(assembler->arena_, prop.key(), prop.value().int_value().value()));
        break;

      case fuchsia_driver_legacy::PropertyValue::Tag::kStrValue:
        assembler->properties_.emplace_back(
            fdf::MakeProperty(assembler->arena_, prop.key(), prop.value().str_value().value()));
        break;

      case fuchsia_driver_legacy::PropertyValue::Tag::kEnumValue:
        assembler->properties_.emplace_back(
            fdf::MakeProperty(assembler->arena_, prop.key(), prop.value().enum_value().value()));
        break;
    }
  }

  // Add the composite value.
  assembler->properties_.emplace_back(fdf::MakeProperty(assembler->arena_, BIND_COMPOSITE, 1));

  // Make the primary fragment first.
  auto fragment =
      CompositeDeviceFragment::Create(descriptor.fragments()[descriptor.primary_fragment_index()]);
  if (fragment.is_error()) {
    return fragment.take_error();
  }
  assembler->fragments_.push_back(std::move(fragment.value()));

  // Make the other fragments.
  for (size_t i = 0; i < descriptor.fragments().size(); i++) {
    if (i == descriptor.primary_fragment_index()) {
      continue;
    }
    auto fragment = CompositeDeviceFragment::Create(descriptor.fragments()[i]);
    if (fragment.is_error()) {
      return fragment.take_error();
    }
    assembler->fragments_.push_back(std::move(fragment.value()));
  }

  return zx::ok(std::move(assembler));
}

std::optional<uint32_t> CompositeDeviceAssembler::BindNode(std::shared_ptr<Node> node) {
  std::optional<uint32_t> matched = std::nullopt;
  uint32_t i = 0;
  for (auto& fragment : fragments_) {
    if (fragment.BindNode(node)) {
      matched = i;
      LOGF(DEBUG, "Found a match for composite device '%s': fragment %s: device '%s'",
           name_.c_str(), std::string(fragment.name()).c_str(),
           node->MakeComponentMoniker().c_str());
      break;
    }

    i++;
  }

  if (!matched.has_value()) {
    return std::nullopt;
  }

  TryToAssemble();
  return matched;
}

void CompositeDeviceAssembler::TryToAssemble() {
  if (assembled_node_.has_value()) {
    return;
  }

  std::vector<std::shared_ptr<Node>> strong_parents;
  std::vector<std::weak_ptr<Node>> parents;
  std::vector<std::string> parents_names;
  for (auto& fragment : fragments_) {
    auto node = fragment.bound_node();
    // A fragment is missing a node, don't assemble.
    if (!node) {
      return;
    }
    parents.push_back(node);
    parents_names.emplace_back(fragment.name());
    strong_parents.push_back(std::move(node));
  }

  auto node =
      Node::CreateCompositeNode(name_, std::move(parents), parents_names, std::move(properties_),
                                node_manager_, dispatcher_, /* is_legacy*/ true);
  if (node.is_error()) {
    return;
  }

  LOGF(INFO, "Built composite device at '%s'", node->MakeComponentMoniker().c_str());

  assembled_node_ = *node;

  // Bind the node we just created.
  node_manager_->Bind(*node.value(), nullptr);
}

void CompositeDeviceAssembler::Inspect(inspect::Node& root) const {
  auto node = root.CreateChild(root.UniqueName("assembler-"));
  node.RecordString("name", name_);

  for (auto& fragment : fragments_) {
    fragment.Inspect(node);
  }

  root.Record(std::move(node));
}

CompositeDeviceManager::CompositeDeviceManager(NodeManager* node_manager,
                                               async_dispatcher_t* dispatcher,
                                               fit::function<void()> rebind_callback)
    : node_manager_(node_manager),
      dispatcher_(dispatcher),
      rebind_callback_(std::move(rebind_callback)) {}

zx_status_t CompositeDeviceManager::AddCompositeDevice(
    std::string name, fuchsia_device_manager::CompositeDeviceDescriptor descriptor) {
  auto assembler = CompositeDeviceAssembler::Create(std::move(name), std::move(descriptor),
                                                    node_manager_, dispatcher_);
  if (assembler.is_error()) {
    return assembler.error_value();
  }
  assemblers_.push_back(std::move(assembler.value()));

  rebind_callback_();
  return ZX_OK;
}

std::vector<fdl::CompositeParent> CompositeDeviceManager::BindNode(std::shared_ptr<Node> node) {
  std::vector<fdl::CompositeParent> result_info;
  for (auto& assembler : assemblers_) {
    std::optional<uint32_t> bound = assembler->BindNode(node);
    if (!bound.has_value()) {
      continue;
    }

    result_info.push_back(fdl::CompositeParent({assembler->GetLegacyCompositeInfo(), bound}));

    // If the node cannot multibind, then it should only be matched with one
    // legacy composite.
    if (!node->can_multibind_composites()) {
      ZX_ASSERT(result_info.size() == 1);
      return result_info;
    }
  }

  return result_info;
}

void CompositeDeviceManager::Publish(component::OutgoingDirectory& outgoing) {
  auto result = outgoing.AddUnmanagedProtocol<fuchsia_device_composite::DeprecatedCompositeCreator>(
      bindings_.CreateHandler(this, dispatcher_, fidl::kIgnoreBindingClosure));
  ZX_ASSERT(result.is_ok());
}

void CompositeDeviceManager::Inspect(inspect::Node& root) const {
  for (auto& assembler : assemblers_) {
    assembler->Inspect(root);
  }
}

std::vector<fdd::wire::CompositeNodeInfo> CompositeDeviceManager::GetCompositeListInfo(
    fidl::AnyArena& arena) const {
  std::vector<fdd::wire::CompositeNodeInfo> composite_list;
  composite_list.reserve(assemblers_.size());
  for (auto& assembler : assemblers_) {
    composite_list.push_back(fidl::ToWire(arena, assembler->GetCompositeInfo()));
  }
  return composite_list;
}

void CompositeDeviceManager::AddCompositeDevice(AddCompositeDeviceRequest& request,
                                                AddCompositeDeviceCompleter::Sync& completer) {
  zx_status_t status = AddCompositeDevice(request.name(), request.args());
  completer.Reply(zx::make_result(status));
}

}  // namespace dfv2
