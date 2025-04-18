// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_DDKTL_INCLUDE_DDKTL_COMPOSITE_NODE_SPEC_H_
#define SRC_LIB_DDKTL_INCLUDE_DDKTL_COMPOSITE_NODE_SPEC_H_

#include <lib/ddk/device.h>
#include <lib/stdcompat/span.h>

#include <fbl/vector.h>

namespace ddk {

class BindRule {
 public:
  static BindRule CreateWithIntList(device_bind_prop_key_t key,
                                    device_bind_rule_condition condition,
                                    cpp20::span<const uint32_t> values) {
    fbl::AllocChecker ac;
    auto bind_prop_values = fbl::Vector<device_bind_prop_value_t>();
    bind_prop_values.reserve(values.size(), &ac);
    ZX_ASSERT(ac.check());
    for (size_t i = 0; i < values.size(); ++i) {
      bind_prop_values[i] = device_bind_prop_int_val(values[i]);
    }

    return BindRule(key, condition, std::move(bind_prop_values));
  }

  static BindRule CreateWithStringList(device_bind_prop_key_t key,
                                       device_bind_rule_condition condition,
                                       cpp20::span<const char*> values) {
    fbl::AllocChecker ac;
    auto bind_prop_values = fbl::Vector<device_bind_prop_value_t>();
    bind_prop_values.reserve(values.size(), &ac);
    ZX_ASSERT(ac.check());
    for (size_t i = 0; i < values.size(); ++i) {
      bind_prop_values[i] = device_bind_prop_str_val(values[i]);
    }

    return BindRule(key, condition, std::move(bind_prop_values));
  }

  BindRule(device_bind_prop_key_t key, device_bind_rule_condition condition,
           device_bind_prop_value_t value) {
    fbl::AllocChecker ac;
    value_data_.push_back(value, &ac);
    ZX_ASSERT(ac.check());
    rule_ = bind_rule_t{
        .key = key,
        .condition = condition,
        .values = value_data_.data(),
        .values_count = std::size(value_data_),
    };
  }

  BindRule(device_bind_prop_key_t key, device_bind_rule_condition condition,
           fbl::Vector<device_bind_prop_value_t> values)
      : value_data_(std::move(values)) {
    fbl::AllocChecker ac;
    value_data_.reserve(values.size(), &ac);
    ZX_ASSERT(ac.check());
    rule_ = bind_rule_t{
        .key = key,
        .condition = condition,
        .values = value_data_.data(),
        .values_count = std::size(value_data_),
    };
  }

  BindRule& operator=(const BindRule& other) {
    // value_data_.clear();
    value_data_.reset();
    for (size_t i = 0; i < other.value_data_.size(); ++i) {
      fbl::AllocChecker ac;
      value_data_.push_back(other.value_data_[i], &ac);
      ZX_ASSERT(ac.check());
    }

    rule_ = bind_rule_t{
        .key = other.rule_.key,
        .condition = other.rule_.condition,
        .values = value_data_.data(),
        .values_count = std::size(value_data_),
    };

    return *this;
  }

  BindRule(const BindRule& other) { *this = other; }

  const bind_rule_t& get() const { return rule_; }

  fbl::Vector<device_bind_prop_value_t> value_data() const {
    fbl::AllocChecker ac;
    fbl::Vector<device_bind_prop_value_t> copy;
    copy.reserve(value_data_.size(), &ac);
    ZX_ASSERT(ac.check());
    for (size_t i = 0; i < value_data_.size(); ++i) {
      copy.push_back(value_data_[i], &ac);
      ZX_ASSERT(ac.check());
    }
    return copy;
  }

 private:
  // Contains the data for bind property values.
  fbl::Vector<device_bind_prop_value_t> value_data_;

  bind_rule_t rule_;
};

// Factory functions to create a BindRule.
// std::string values passed in the functions must outlive the returned value.
inline BindRule MakeAcceptBindRule(uint32_t key, uint32_t val) {
  return BindRule(device_bind_prop_int_key(key), DEVICE_BIND_RULE_CONDITION_ACCEPT,
                  device_bind_prop_int_val(val));
}

inline BindRule MakeAcceptBindRule(const std::string& key, uint32_t val) {
  return BindRule(device_bind_prop_str_key(key.c_str()), DEVICE_BIND_RULE_CONDITION_ACCEPT,
                  device_bind_prop_int_val(val));
}

inline BindRule MakeAcceptBindRule(const char* key, uint32_t val) {
  return BindRule(device_bind_prop_str_key(key), DEVICE_BIND_RULE_CONDITION_ACCEPT,
                  device_bind_prop_int_val(val));
}

inline BindRule MakeAcceptBindRule(const std::string& key, bool val) {
  return BindRule(device_bind_prop_str_key(key.c_str()), DEVICE_BIND_RULE_CONDITION_ACCEPT,
                  device_bind_prop_bool_val(val));
}

inline BindRule MakeAcceptBindRule(const char* key, bool val) {
  return BindRule(device_bind_prop_str_key(key), DEVICE_BIND_RULE_CONDITION_ACCEPT,
                  device_bind_prop_bool_val(val));
}

inline BindRule MakeAcceptBindRule(const std::string& key, const std::string& val) {
  return BindRule(device_bind_prop_str_key(key.c_str()), DEVICE_BIND_RULE_CONDITION_ACCEPT,
                  device_bind_prop_str_val(val.c_str()));
}

inline BindRule MakeAcceptBindRule(const char* key, const std::string& val) {
  return BindRule(device_bind_prop_str_key(key), DEVICE_BIND_RULE_CONDITION_ACCEPT,
                  device_bind_prop_str_val(val.c_str()));
}

inline BindRule MakeAcceptBindRule(const std::string& key, const char* val) {
  return BindRule(device_bind_prop_str_key(key.c_str()), DEVICE_BIND_RULE_CONDITION_ACCEPT,
                  device_bind_prop_str_val(val));
}

inline BindRule MakeAcceptBindRule(const char* key, const char* val) {
  return BindRule(device_bind_prop_str_key(key), DEVICE_BIND_RULE_CONDITION_ACCEPT,
                  device_bind_prop_str_val(val));
}

inline BindRule MakeRejectBindRule(uint32_t key, uint32_t val) {
  return BindRule(device_bind_prop_int_key(key), DEVICE_BIND_RULE_CONDITION_REJECT,
                  device_bind_prop_int_val(val));
}

inline BindRule MakeRejectBindRule(const std::string& key, uint32_t val) {
  return BindRule(device_bind_prop_str_key(key.c_str()), DEVICE_BIND_RULE_CONDITION_REJECT,
                  device_bind_prop_int_val(val));
}

inline BindRule MakeRejectBindRule(const char* key, uint32_t val) {
  return BindRule(device_bind_prop_str_key(key), DEVICE_BIND_RULE_CONDITION_REJECT,
                  device_bind_prop_int_val(val));
}

inline BindRule MakeRejectBindRule(const std::string& key, bool val) {
  return BindRule(device_bind_prop_str_key(key.c_str()), DEVICE_BIND_RULE_CONDITION_REJECT,
                  device_bind_prop_bool_val(val));
}

inline BindRule MakeRejectBindRule(const char* key, bool val) {
  return BindRule(device_bind_prop_str_key(key), DEVICE_BIND_RULE_CONDITION_REJECT,
                  device_bind_prop_bool_val(val));
}

inline BindRule MakeRejectBindRule(const std::string& key, const std::string& val) {
  return BindRule(device_bind_prop_str_key(key.c_str()), DEVICE_BIND_RULE_CONDITION_REJECT,
                  device_bind_prop_str_val(val.c_str()));
}

inline BindRule MakeRejectBindRule(const char* key, const std::string& val) {
  return BindRule(device_bind_prop_str_key(key), DEVICE_BIND_RULE_CONDITION_REJECT,
                  device_bind_prop_str_val(val.c_str()));
}

inline BindRule MakeRejectBindRule(const std::string& key, const char* val) {
  return BindRule(device_bind_prop_str_key(key.c_str()), DEVICE_BIND_RULE_CONDITION_REJECT,
                  device_bind_prop_str_val(val));
}

inline BindRule MakeRejectBindRule(const char* key, const char* val) {
  return BindRule(device_bind_prop_str_key(key), DEVICE_BIND_RULE_CONDITION_REJECT,
                  device_bind_prop_str_val(val));
}

inline BindRule MakeAcceptBindRuleList(uint32_t key, cpp20::span<const uint32_t> values) {
  return ddk::BindRule::CreateWithIntList(device_bind_prop_int_key(key),
                                          DEVICE_BIND_RULE_CONDITION_ACCEPT, values);
}

inline BindRule MakeAcceptBindRuleList(const std::string& key, cpp20::span<const uint32_t> values) {
  return ddk::BindRule::CreateWithIntList(device_bind_prop_str_key(key.c_str()),
                                          DEVICE_BIND_RULE_CONDITION_ACCEPT, values);
}

inline BindRule MakeAcceptBindRuleList(const char* key, cpp20::span<const uint32_t> values) {
  return ddk::BindRule::CreateWithIntList(device_bind_prop_str_key(key),
                                          DEVICE_BIND_RULE_CONDITION_ACCEPT, values);
}

inline BindRule MakeAcceptBindRuleList(const std::string& key, cpp20::span<const char*> values) {
  return ddk::BindRule::CreateWithStringList(device_bind_prop_str_key(key.c_str()),
                                             DEVICE_BIND_RULE_CONDITION_ACCEPT, values);
}

inline BindRule MakeAcceptBindRuleList(const char* key, cpp20::span<const char*> values) {
  return ddk::BindRule::CreateWithStringList(device_bind_prop_str_key(key),
                                             DEVICE_BIND_RULE_CONDITION_ACCEPT, values);
}

inline BindRule MakeRejectBindRuleList(uint32_t key, cpp20::span<const uint32_t> values) {
  return ddk::BindRule::CreateWithIntList(device_bind_prop_int_key(key),
                                          DEVICE_BIND_RULE_CONDITION_REJECT, values);
}

inline BindRule MakeRejectBindRuleList(const std::string& key, cpp20::span<const uint32_t> values) {
  return ddk::BindRule::CreateWithIntList(device_bind_prop_str_key(key.c_str()),
                                          DEVICE_BIND_RULE_CONDITION_REJECT, values);
}

inline BindRule MakeRejectBindRuleList(const char* key, cpp20::span<const uint32_t> values) {
  return ddk::BindRule::CreateWithIntList(device_bind_prop_str_key(key),
                                          DEVICE_BIND_RULE_CONDITION_REJECT, values);
}

inline BindRule MakeRejectBindRuleList(const std::string& key, cpp20::span<const char*> values) {
  return ddk::BindRule::CreateWithStringList(device_bind_prop_str_key(key.c_str()),
                                             DEVICE_BIND_RULE_CONDITION_REJECT, values);
}

inline BindRule MakeRejectBindRuleList(const char* key, cpp20::span<const char*> values) {
  return ddk::BindRule::CreateWithStringList(device_bind_prop_str_key(key),
                                             DEVICE_BIND_RULE_CONDITION_REJECT, values);
}

// Factory functions to create a device_bind_prop_t.
// std::string values passed in the functions must outlive the returned value.
inline device_bind_prop_t MakeProperty(uint32_t key, uint32_t val) {
  return {device_bind_prop_int_key(key), device_bind_prop_int_val(val)};
}

inline device_bind_prop_t MakeProperty(const std::string& key, uint32_t val) {
  return {device_bind_prop_str_key(key.c_str()), device_bind_prop_int_val(val)};
}

inline device_bind_prop_t MakeProperty(const char* key, uint32_t val) {
  return {device_bind_prop_str_key(key), device_bind_prop_int_val(val)};
}

inline device_bind_prop_t MakeProperty(const std::string& key, bool val) {
  return {device_bind_prop_str_key(key.c_str()), device_bind_prop_bool_val(val)};
}

inline device_bind_prop_t MakeProperty(const char* key, bool val) {
  return {device_bind_prop_str_key(key), device_bind_prop_bool_val(val)};
}

inline device_bind_prop_t MakeProperty(const std::string& key, const std::string& val) {
  return {device_bind_prop_str_key(key.c_str()), device_bind_prop_str_val(val.c_str())};
}

inline device_bind_prop_t MakeProperty(const char* key, const std::string& val) {
  return {device_bind_prop_str_key(key), device_bind_prop_str_val(val.c_str())};
}

inline device_bind_prop_t MakeProperty(const std::string& key, const char* val) {
  return {device_bind_prop_str_key(key.c_str()), device_bind_prop_str_val(val)};
}

inline device_bind_prop_t MakeProperty(const char* key, const char* val) {
  return {device_bind_prop_str_key(key), device_bind_prop_str_val(val)};
}

class CompositeNodeSpec {
 public:
  CompositeNodeSpec(cpp20::span<const BindRule> bind_rules,
                    cpp20::span<const device_bind_prop_t> properties) {
    AddParentSpec(bind_rules, properties);
    specs_.parents = parent_specs_.data();
  }

  CompositeNodeSpec& operator=(const CompositeNodeSpec& other) {
    specs_ = other.specs_;

    parent_specs_.reset();
    bind_rules_data_.reset();
    bind_rules_values_data_.reset();
    properties_data_.reset();
    for (size_t i = 0; i < other.parent_specs_.size(); ++i) {
      AddParentSpec(other.parent_specs_[i]);
    }

    specs_.parents = parent_specs_.data();
    return *this;
  }

  CompositeNodeSpec(const CompositeNodeSpec& other) { *this = other; }

  // Add a node to |parent_specs_| and store the property data in |prop_data_|.
  CompositeNodeSpec& AddParentSpec(cpp20::span<const BindRule> rules,
                                   cpp20::span<const device_bind_prop_t> properties) {
    fbl::AllocChecker ac;
    auto bind_rule_count = rules.size();
    auto bind_rules = fbl::Vector<bind_rule_t>();
    bind_rules.reserve(bind_rule_count, &ac);
    for (size_t i = 0; i < bind_rule_count; i++) {
      bind_rules[i] = rules[i].get();

      auto bind_rule_values = rules[i].value_data();
      bind_rules[i].values = bind_rule_values.data();
      bind_rules_values_data_.push_back(std::move(bind_rule_values), &ac);
      ZX_ASSERT(ac.check());
    }

    auto prop_count = properties.size();
    auto props = fbl::Vector<device_bind_prop_t>();
    props.reserve(prop_count, &ac);
    for (size_t i = 0; i < prop_count; i++) {
      props.push_back(properties[i], &ac);
      ZX_ASSERT(ac.check());
    }

    parent_specs_.push_back(
        parent_spec_t{
            .bind_rules = bind_rules.data(),
            .bind_rule_count = bind_rule_count,
            .properties = props.data(),
            .property_count = prop_count,
        },
        &ac);
    ZX_ASSERT(ac.check());

    bind_rules_data_.push_back(std::move(bind_rules), &ac);
    ZX_ASSERT(ac.check());
    properties_data_.push_back(std::move(props), &ac);
    ZX_ASSERT(ac.check());

    specs_.parents = parent_specs_.data();
    specs_.parent_count = std::size(parent_specs_);

    return *this;
  }

  const composite_node_spec_t& get() const { return specs_; }

 private:
  // Add a node to |parent_specs_| and store the property data in |prop_data_|.
  void AddParentSpec(const parent_spec_t& parent) {
    fbl::AllocChecker ac;
    auto bind_rule_count = parent.bind_rule_count;
    auto bind_rules = fbl::Vector<bind_rule_t>();
    bind_rules.reserve(bind_rule_count, &ac);
    for (size_t i = 0; i < bind_rule_count; i++) {
      auto parent_rules = parent.bind_rules[i];
      auto bind_rule_values = fbl::Vector<device_bind_prop_value_t>();
      bind_rule_values.reserve(parent_rules.values_count, &ac);
      for (size_t k = 0; k < parent_rules.values_count; k++) {
        bind_rule_values[k] = parent_rules.values[k];
      }

      bind_rules[i] = parent_rules;
      bind_rules[i].values = bind_rule_values.data();
      bind_rules_values_data_.push_back(std::move(bind_rule_values), &ac);
      ZX_ASSERT(ac.check());
    }

    auto property_count = parent.property_count;
    auto properties = fbl::Vector<device_bind_prop_t>();
    properties.reserve(property_count, &ac);
    for (size_t i = 0; i < property_count; i++) {
      properties.push_back(parent.properties[i], &ac);
      ZX_ASSERT(ac.check());
    }

    parent_specs_.push_back(
        parent_spec_t{
            .bind_rules = bind_rules.data(),
            .bind_rule_count = bind_rule_count,
            .properties = properties.data(),
            .property_count = property_count,
        },
        &ac);
    ZX_ASSERT(ac.check());
    specs_.parents = parent_specs_.data();
    specs_.parent_count = std::size(parent_specs_);

    bind_rules_data_.push_back(std::move(bind_rules), &ac);
    ZX_ASSERT(ac.check());
    properties_data_.push_back(std::move(properties), &ac);
    ZX_ASSERT(ac.check());
  }

  fbl::Vector<parent_spec_t> parent_specs_;

  // Stores all the bind rules data in |parent_specs_|.
  fbl::Vector<fbl::Vector<bind_rule_t>> bind_rules_data_;

  // Store all bind rule values data in |parent_specs_|.
  fbl::Vector<fbl::Vector<device_bind_prop_value_t>> bind_rules_values_data_;

  // Stores all properties data in |parent_specs_|.
  fbl::Vector<fbl::Vector<device_bind_prop_t>> properties_data_;

  composite_node_spec_t specs_ = {};
};

}  // namespace ddk

#endif  // SRC_LIB_DDKTL_INCLUDE_DDKTL_COMPOSITE_NODE_SPEC_H_
