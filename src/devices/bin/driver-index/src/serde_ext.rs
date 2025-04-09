// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_driver_framework as fdf;
use serde::ser::SerializeSeq;
use serde::{Deserialize, Serialize};

// Macro to generate a (de)serializer for option types with a local serde placeholder structure for
// an external type. Use it like so:
//
//   #[derive(Serialize, Deserialize)]
//   #[derive(remote = "MyType")]
//   struct MyTypeDef {
//     foo: String,
//   }
//   option_encoding!(OptionMyTypeDef, MyType, "MyTypeDef");
//
//   #[derive(Serialize, Deserialize)]
//   struct MyOtherTypeDef {
//      // A `MyType` can be (de)serialized with "MyTypeDef".
//      #[serde(with = "MyTypeDef")]
//      my_required_type: MyType,
//
//      // A `Option<MyType>` can be (de)serialized with "OptionMyTypeDef" which we declared above.
//      #[serde(with = "OptionMyTypeDef")]
//      my_optional_type: Option<MyType>,
//   }
macro_rules! option_encoding {
    ($encoding:ident, $remote_type:ty, $local_type:expr) => {
        #[allow(explicit_outlives_requirements)]
        mod $encoding {
            use super::*;
            use serde::{Deserialize, Deserializer, Serialize, Serializer};

            pub fn serialize<S>(
                value: &Option<$remote_type>,
                serializer: S,
            ) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                #[derive(Serialize)]
                struct Wrapper<'a>(#[serde(with = $local_type)] &'a $remote_type);
                value.as_ref().map(Wrapper).serialize(serializer)
            }

            pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<$remote_type>, D::Error>
            where
                D: Deserializer<'de>,
            {
                #[derive(Deserialize)]
                struct Wrapper(#[serde(with = $local_type)] $remote_type);
                let helper = Option::deserialize(deserializer)?;
                Ok(helper.map(|Wrapper(external)| external))
            }
        }
    };
}

// Macro to generate a (de)serializer for vector types with a local serde placeholder structure for
// an external type. Use it like so:
//
//   #[derive(Serialize, Deserialize)]
//   #[derive(remote = "MyType")]
//   struct MyTypeDef {
//     foo: String,
//   }
//   vector_encoding!(VectorMyTypeDef, MyType, "MyTypeDef");
//
//   #[derive(Serialize, Deserialize)]
//   struct MyOtherTypeDef {
//      // A `MyType` can be (de)serialized with "MyTypeDef".
//      #[serde(with = "MyTypeDef")]
//      my_required_type: MyType,
//
//      // A `Vec<MyType>` can be (de)serialized with "VectorMyTypeDef" which we declared above.
//      #[serde(with = "VectorMyTypeDef")]
//      my_vector_type: Vec<MyType>,
//   }
macro_rules! vector_encoding {
    ($encoding:ident, $remote_type:ty, $local_type:expr) => {
        #[allow(explicit_outlives_requirements)]
        mod $encoding {
            use super::*;
            use serde::{Deserialize, Deserializer, Serialize, Serializer};

            pub fn serialize<S>(value: &Vec<$remote_type>, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                #[derive(Serialize)]
                struct Wrapper<'a>(#[serde(with = $local_type)] &'a $remote_type);
                let mut seq = serializer.serialize_seq(Some(value.len()))?;
                for item in value {
                    seq.serialize_element(&Wrapper(item))?;
                }
                seq.end()
            }

            pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<$remote_type>, D::Error>
            where
                D: Deserializer<'de>,
            {
                #[derive(Deserialize)]
                struct Wrapper(#[serde(with = $local_type)] $remote_type);
                let helper = Vec::deserialize(deserializer)?;
                Ok(helper
                    .into_iter()
                    .map(|Wrapper(external)| external)
                    .collect::<Vec<$remote_type>>())
            }
        }
    };
}

// Macro to generate a (de)serializer for option of vector types with a local serde placeholder
// structure for an external type. Use it like so:
//
//   #[derive(Serialize, Deserialize)]
//   #[derive(remote = "MyType")]
//   struct MyTypeDef {
//     foo: String,
//   }
//
//   // Define the vector first, then wrap with option.
//   vector_encoding!(VectorMyTypeDef, MyType, "MyTypeDef");
//   option_of_vec_encoding!(OptionVectorMyTypeDef, "VectorMyTypeDef", MyType)
//
//   #[derive(Serialize, Deserialize)]
//   struct MyOtherTypeDef {
//      // A `MyType` can be (de)serialized with "MyTypeDef".
//      #[serde(with = "MyTypeDef")]
//      my_required_type: MyType,
//
//      // A `Option<Vec<MyType>>` can be (de)serialized with "OptionVectorMyTypeDef" which we
//      // declared above.
//      #[serde(with = "OptionVectorMyTypeDef")]
//      my_optional_type: Option<Vec<MyType>>,
//   }
macro_rules! option_of_vec_encoding {
    ($encoding:ident, $inner_mod:expr, $remote_inner_type:ty) => {
        #[allow(explicit_outlives_requirements)]
        mod $encoding {
            use super::*;
            use serde::{Deserialize, Deserializer, Serialize, Serializer};

            pub fn serialize<S>(
                value: &Option<Vec<$remote_inner_type>>,
                serializer: S,
            ) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                #[derive(Serialize)]
                struct Wrapper<'a>(#[serde(with = $inner_mod)] &'a Vec<$remote_inner_type>);
                value.as_ref().map(Wrapper).serialize(serializer)
            }

            pub fn deserialize<'de, D>(
                deserializer: D,
            ) -> Result<Option<Vec<$remote_inner_type>>, D::Error>
            where
                D: Deserializer<'de>,
            {
                #[derive(Deserialize)]
                struct Wrapper(#[serde(with = $inner_mod)] Vec<$remote_inner_type>);
                let helper = Option::deserialize(deserializer)?;
                Ok(helper.map(|Wrapper(external)| external))
            }
        }
    };
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "fidl::marker::SourceBreaking")]
pub struct SourceBreakingDef;

#[derive(Serialize, Deserialize)]
#[serde(remote = "fdf::NodePropertyKey")]
pub enum NodePropertyKeyDef {
    IntValue(u32),
    StringValue(String),
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "fdf::NodePropertyValue")]
pub enum NodePropertyValueDef {
    IntValue(u32),
    StringValue(String),
    BoolValue(bool),
    EnumValue(String),
    #[doc(hidden)]
    __SourceBreaking {
        unknown_ordinal: u64,
    },
}

option_encoding!(opt_composite_node_spec, fdf::CompositeNodeSpec, "CompositeNodeSpecDef");
option_encoding!(opt_composite_driver_match, fdf::CompositeDriverMatch, "CompositeDriverMatchDef");
option_encoding!(opt_driver_package_type, fdf::DriverPackageType, "DriverPackageTypeDef");
option_encoding!(opt_driver_info, fdf::DriverInfo, "DriverInfoDef");
option_encoding!(opt_composite_driver_info, fdf::CompositeDriverInfo, "CompositeDriverInfoDef");

vector_encoding!(vector_node_property_value, fdf::NodePropertyValue, "NodePropertyValueDef");
vector_encoding!(vector_bind_rule, fdf::BindRule, "BindRuleDef");
vector_encoding!(vector_bind_rule2, fdf::BindRule2, "BindRule2Def");
vector_encoding!(vector_node_property, fdf::NodeProperty, "NodePropertyDef");
vector_encoding!(vector_node_property2, fdf::NodeProperty2, "NodeProperty2Def");
vector_encoding!(vector_parent_spec, fdf::ParentSpec, "ParentSpecDef");
vector_encoding!(vector_parent_spec2, fdf::ParentSpec2, "ParentSpec2Def");
vector_encoding!(vector_device_category, fdf::DeviceCategory, "DeviceCategoryDef");

option_of_vec_encoding!(opt_vec_parent_spec, "vector_parent_spec", fdf::ParentSpec);
option_of_vec_encoding!(opt_vec_parent_spec2, "vector_parent_spec2", fdf::ParentSpec2);
option_of_vec_encoding!(opt_vec_device_category, "vector_device_category", fdf::DeviceCategory);

#[derive(Serialize, Deserialize)]
#[serde(remote = "fdf::Condition")]
pub enum ConditionDef {
    Unknown = 0,
    Accept = 1,
    Reject = 2,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "fdf::NodeProperty")]
pub struct NodePropertyDef {
    /// Key for the property.
    #[serde(with = "NodePropertyKeyDef")]
    pub key: fdf::NodePropertyKey,
    /// Value for the property.
    #[serde(with = "NodePropertyValueDef")]
    pub value: fdf::NodePropertyValue,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "fdf::NodeProperty2")]
pub struct NodeProperty2Def {
    /// Key for the property.
    pub key: String,
    /// Value for the property.
    #[serde(with = "NodePropertyValueDef")]
    pub value: fdf::NodePropertyValue,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "fdf::BindRule")]
pub struct BindRuleDef {
    /// Property key.
    #[serde(with = "NodePropertyKeyDef")]
    pub key: fdf::NodePropertyKey,
    /// Condition for evaluating the property values in
    /// the matching process. The values must be ACCEPT
    /// or REJECT.
    #[serde(with = "ConditionDef")]
    pub condition: fdf::Condition,
    /// A list of property values. Must not be empty. The property
    /// values must be the same type.
    #[serde(with = "vector_node_property_value")]
    pub values: Vec<fdf::NodePropertyValue>,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "fdf::BindRule2")]
pub struct BindRule2Def {
    /// Property key.
    pub key: String,
    /// Condition for evaluating the property values in
    /// the matching process. The values must be ACCEPT
    /// or REJECT.
    #[serde(with = "ConditionDef")]
    pub condition: fdf::Condition,
    /// A list of property values. Must not be empty. The property
    /// values must be the same type.
    #[serde(with = "vector_node_property_value")]
    pub values: Vec<fdf::NodePropertyValue>,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "fdf::ParentSpec")]
pub struct ParentSpecDef {
    /// Parent's bind rules. Property keys must be unique. Must not be empty.
    #[serde(with = "vector_bind_rule")]
    pub bind_rules: Vec<fdf::BindRule>,
    /// Properties for matching against a composite driver's bind rules.
    /// Keys must be unique.
    #[serde(with = "vector_node_property")]
    pub properties: Vec<fdf::NodeProperty>,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "fdf::ParentSpec2")]
pub struct ParentSpec2Def {
    /// Parent's bind rules. Property keys must be unique. Must not be empty.
    #[serde(with = "vector_bind_rule2")]
    pub bind_rules: Vec<fdf::BindRule2>,
    /// Properties for matching against a composite driver's bind rules.
    /// Keys must be unique.
    #[serde(with = "vector_node_property2")]
    pub properties: Vec<fdf::NodeProperty2>,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "fdf::CompositeNodeSpec")]
pub struct CompositeNodeSpecDef {
    /// The composite node spec's name.
    pub name: Option<String>,
    /// The nodes in the composite node spec. Must not be empty. The first node is
    /// the primary node.
    #[serde(with = "opt_vec_parent_spec")]
    pub parents: Option<Vec<fdf::ParentSpec>>,
    /// The nodes in the composite node spec. Must not be empty. The first node is
    /// the primary node.
    #[serde(with = "opt_vec_parent_spec2")]
    pub parents2: Option<Vec<fdf::ParentSpec2>>,
    #[doc(hidden)]
    #[serde(with = "SourceBreakingDef")]
    pub __source_breaking: fidl::marker::SourceBreaking,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "fdf::DriverPackageType")]
pub enum DriverPackageTypeDef {
    /// BOOT packages are inside the Zircon boot image.
    Boot,
    /// BASE packages are included in the Fuchsia build as static local packages.
    Base,
    /// CACHED packages are BASE packages that can be updated during a resolve if a full package
    /// resolver is available.
    Cached,
    /// UNIVERSE packages get onto the device only when resolved by the full package resolver.
    Universe,
    #[doc(hidden)]
    __SourceBreaking { unknown_ordinal: u8 },
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "fdf::DeviceCategory")]
pub struct DeviceCategoryDef {
    pub category: Option<String>,
    pub subcategory: Option<String>,
    #[doc(hidden)]
    #[serde(with = "SourceBreakingDef")]
    pub __source_breaking: fidl::marker::SourceBreaking,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "fdf::DriverInfo")]
pub struct DriverInfoDef {
    /// URL of the driver component.
    pub url: Option<String>,
    /// Name of the driver, taken from the first field of the `ZIRCON_DRIVER`
    /// macro in the driver.
    pub name: Option<String>,
    /// If this is true then the driver should be colocated in its parent's DriverHost.
    pub colocate: Option<bool>,
    /// The type of package this driver is in.
    #[serde(with = "opt_driver_package_type")]
    pub package_type: Option<fdf::DriverPackageType>,
    /// If this is true then the driver is a fallback driver. Fallback drivers have a
    /// lesser priority for binding, so they will only be chosen for binding if there
    /// is no non-fallback driver that has matched.
    pub is_fallback: Option<bool>,
    /// Device categories
    #[serde(with = "opt_vec_device_category")]
    pub device_categories: Option<Vec<fdf::DeviceCategory>>,
    /// Bind rules which declare set of constraints to evaluate in order to
    /// determine whether the driver indexer should bind this driver to a
    /// device.
    pub bind_rules_bytecode: Option<Vec<u8>>,
    /// The version of the driver framework that this driver is using.
    /// Supported values are 1 (DFv1) and 2 (DFv2).
    /// If not provided, 1 is the assumed version.
    pub driver_framework_version: Option<u8>,
    /// Whether the driver is disabled. If true, this driver is not chosen to bind to nodes.
    pub is_disabled: Option<bool>,
    #[doc(hidden)]
    #[serde(with = "SourceBreakingDef")]
    pub __source_breaking: fidl::marker::SourceBreaking,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "fdf::CompositeDriverInfo")]
pub struct CompositeDriverInfoDef {
    /// The name of the composite as specified in the driver's composite bind rules.
    pub composite_name: Option<String>,
    /// General information for the driver.
    #[serde(with = "opt_driver_info")]
    pub driver_info: Option<fdf::DriverInfo>,
    #[doc(hidden)]
    #[serde(with = "SourceBreakingDef")]
    pub __source_breaking: fidl::marker::SourceBreaking,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "fdf::CompositeDriverMatch")]
pub struct CompositeDriverMatchDef {
    /// Information for the composite driver that has matched.
    #[serde(with = "opt_composite_driver_info")]
    pub composite_driver: Option<fdf::CompositeDriverInfo>,
    /// A list of all the parent names, ordered by index.
    /// These names come from the node definitions in the driver's composite bind rules.
    pub parent_names: Option<Vec<String>>,
    /// The primary node index. Identified by the primary node in the driver's
    /// composite bind rules.
    pub primary_parent_index: Option<u32>,
    #[doc(hidden)]
    #[serde(with = "SourceBreakingDef")]
    pub __source_breaking: fidl::marker::SourceBreaking,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "fdf::CompositeInfo")]
pub struct CompositeInfoDef {
    /// The spec information that this composite node spec was created with.
    #[serde(with = "opt_composite_node_spec")]
    pub spec: Option<fdf::CompositeNodeSpec>,
    /// Information for the node spec that is available only when a driver
    /// has matched to the properties in this spec's parents.
    #[serde(with = "opt_composite_driver_match")]
    pub matched_driver: Option<fdf::CompositeDriverMatch>,
    #[doc(hidden)]
    #[serde(with = "SourceBreakingDef")]
    pub __source_breaking: fidl::marker::SourceBreaking,
}
