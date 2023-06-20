// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v2/composite_assembler.h"

#include <lib/driver/component/cpp/node_add_args.h>

#include "src/devices/lib/log/log.h"
#include "src/lib/storage/vfs/cpp/service.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}  //  namespace fdf

namespace fdd = fuchsia_driver_development;
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

fdd::LegacyCompositeFragmentInfo CompositeDeviceFragment::GetCompositeFragmentInfo() const {
  fdd::LegacyCompositeFragmentInfo fragment_info;
  fragment_info.name() = name_;

  auto bound_node = bound_node_.lock();
  if (auto bound_node = bound_node_.lock()) {
    fragment_info.device() = bound_node->MakeTopologicalPath();
  }

  std::vector<fdm::BindInstruction> bind_rules(bind_rules_.size());
  for (size_t i = 0; i < bind_rules_.size(); i++) {
    bind_rules[i] = fdm::BindInstruction{{
        .op = bind_rules_[i].op,
        .arg = bind_rules_[i].arg,
        .debug = bind_rules_[i].debug,
    }};
  }
  fragment_info.bind_rules() = std::move(bind_rules);
  return fragment_info;
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

fdd::CompositeInfo CompositeDeviceAssembler::GetCompositeInfo() const {
  auto info = fdd::CompositeInfo();
  info.name() = name_;

  // The first fragment is always the primary index.
  info.primary_index() = 0;

  std::vector<fdd::LegacyCompositeFragmentInfo> fragments;
  fragments.reserve(fragments_.size());
  for (auto& fragment : fragments_) {
    fragments.push_back(fragment.GetCompositeFragmentInfo());
  }
  info.node_info() = fdd::CompositeNodeInfo::WithLegacy(fdd::LegacyCompositeNodeInfo{{
      .fragments = std::move(fragments),
  }});

  if (!assembled_node_.has_value()) {
    return info;
  }

  auto node_ptr = assembled_node_->lock();
  if (auto node_ptr = assembled_node_->lock()) {
    info.driver() = node_ptr->driver_url();
    info.topological_path() = node_ptr->MakeTopologicalPath();
  }

  return info;
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
      case fuchsia_device_manager::PropertyValue::Tag::kBoolValue:
        assembler->properties_.emplace_back(
            fdf::MakeProperty(assembler->arena_, prop.key(), prop.value().bool_value().value()));
        break;

      case fuchsia_device_manager::PropertyValue::Tag::kIntValue:
        assembler->properties_.emplace_back(
            fdf::MakeProperty(assembler->arena_, prop.key(), prop.value().int_value().value()));
        break;

      case fuchsia_device_manager::PropertyValue::Tag::kStrValue:
        assembler->properties_.emplace_back(
            fdf::MakeProperty(assembler->arena_, prop.key(), prop.value().str_value().value()));
        break;

      case fuchsia_device_manager::PropertyValue::Tag::kEnumValue:
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

bool CompositeDeviceAssembler::BindNode(std::shared_ptr<Node> node) {
  bool matched = false;
  for (auto& fragment : fragments_) {
    if (fragment.BindNode(node)) {
      matched = true;
      LOGF(INFO, "Found a match for composite device '%s': fragment %s: device '%s'", name_.c_str(),
           std::string(fragment.name()).c_str(), node->MakeComponentMoniker().c_str());
      break;
    }
  }

  if (!matched) {
    return false;
  }

  TryToAssemble();
  return true;
}

void CompositeDeviceAssembler::TryToAssemble() {
  if (assembled_node_.has_value()) {
    return;
  }

  std::vector<std::shared_ptr<Node>> strong_parents;
  std::vector<Node*> parents;
  std::vector<std::string> parents_names;
  for (auto& fragment : fragments_) {
    auto node = fragment.bound_node();
    // A fragment is missing a node, don't assemble.
    if (!node) {
      return;
    }
    parents.push_back(node.get());
    parents_names.emplace_back(fragment.name());
    strong_parents.push_back(std::move(node));
  }

  auto node = Node::CreateCompositeNode(name_, std::move(parents), parents_names,
                                        std::move(properties_), node_manager_, dispatcher_);
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

bool CompositeDeviceManager::BindNode(std::shared_ptr<Node> node) {
  bool did_match = false;
  for (auto& assembler : assemblers_) {
    if (assembler->BindNode(node)) {
      // We do not break here because DFv1 composites allow for MULTIBIND.
      // For example, the sysmem fragment can match multiple composite devices.
      // To support that, nodes can bind to multiple composite devices.
      did_match = true;
    }
  }
  return did_match;
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

std::vector<fdd::wire::CompositeInfo> CompositeDeviceManager::GetCompositeListInfo(
    fidl::AnyArena& arena) const {
  std::vector<fdd::wire::CompositeInfo> composite_list;
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
