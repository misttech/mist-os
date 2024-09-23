// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow as _;
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::fs::read_to_string;
use std::path::Path;
use tee_internal::binding::{TEE_Identity, TEE_UUID};
use thiserror::Error;
use uuid::Uuid;

// Maps to GlobalPlatform TEE Internal Core API Section 4.4: Property Access Functions
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PropType {
    BinaryBlock,
    UnsignedInt32,
    UnsignedInt64,
    Boolean,
    Uuid,
    Identity,
    String,
}

#[derive(Clone, Debug, Error)]
pub enum PropertyError {
    #[error("Bad format: Unable to parse type: {prop_type:?} from value: {value}")]
    BadFormat { prop_type: PropType, value: String },
    #[error("Item not found: {name}")]
    ItemNotFound { name: String },
    // Callers following Internal Core API spec should panic on this error.
    #[error("Generic TeeProperty error: {msg}")]
    Generic { msg: String },
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TeeProperty {
    name: String,
    prop_type: PropType,
    value: String,
}

pub type TeeProperties = Vec<TeeProperty>;

// Maps to GlobalPlatform TEE Internal Core API Section 4.2.2: Login Types
#[derive(Debug, Deserialize, Serialize)]
pub enum LoginType {
    // Client
    PublicTeecLoginPublic,
    UserTeecLoginUser,
    GroupTeecLoginGroup,
    ApplicationTeecLoginApplication,
    ApplicationUserTeecLoginApplicationUser,
    ApplicationGroup,
    // TA
    TrustedApp,
}

impl LoginType {
    fn to_u32(&self) -> u32 {
        match self {
            LoginType::PublicTeecLoginPublic => 0x00000000,
            LoginType::UserTeecLoginUser => 0x00000001,
            LoginType::GroupTeecLoginGroup => 0x00000002,
            LoginType::ApplicationTeecLoginApplication => 0x00000004,
            LoginType::ApplicationUserTeecLoginApplicationUser => 0x00000005,
            LoginType::ApplicationGroup => 0x00000006,
            LoginType::TrustedApp => 0xF0000000,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TeeIdentity {
    login_type: LoginType,
    // This assumes the UUID is serialized in standard string format rather than the TEE_UUID struct
    // e.g. 5b9e0e40-2636-11e1-ad9e-0002a5d5c51b. An extra parsing step will be required.
    uuid: String,
}

// Maps to GlobalPlatform TEE Internal Core API Section 4.2.4: Property Set Pseudo-Handles
// TeePropSetTeeImplementation = 0xFFFFFFFD,
// TeePropSetCurrentClient = 0xFFFFFFFE,
// TeePropsetCurrentTA = 0xFFFFFFFF,
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PropSetType {
    TeeImplementation,
    CurrentClient,
    CurrentTA,
}

pub type PropertiesMap = IndexMap<String, (PropType, String)>;

#[allow(dead_code)]
pub struct PropSet {
    prop_set_type: PropSetType,
    properties: PropertiesMap,
}

impl PropSet {
    #[cfg(test)]
    pub(crate) fn new(prop_set_type: PropSetType, properties: PropertiesMap) -> Self {
        Self { prop_set_type, properties }
    }

    pub fn from_config_file(
        config_path: &Path,
        prop_set: PropSetType,
    ) -> Result<Self, PropertyError> {
        match read_to_string(config_path) {
            Ok(config_string) => Self::from_config_string(&config_string, prop_set),
            Err(e) => Err(PropertyError::Generic { msg: e.to_string() }),
        }
    }

    pub fn from_config_string(
        config_string: &str,
        prop_set: PropSetType,
    ) -> Result<Self, PropertyError> {
        let props: TeeProperties = match serde_json5::from_str(config_string) {
            Ok(tee_props) => tee_props,
            Err(e) => return Err(PropertyError::Generic { msg: e.to_string() }),
        };
        let mut property_map = IndexMap::new();

        for property in props {
            property_map.insert(property.name, (property.prop_type, property.value));
        }

        Ok(Self { prop_set_type: prop_set, properties: property_map })
    }

    fn get_value(&self, prop_name: String) -> Result<String, PropertyError> {
        match self.properties.get(&prop_name) {
            Some((_, val)) => Ok(val.clone()),
            None => Err(PropertyError::ItemNotFound { name: prop_name }),
        }
    }

    pub fn get_string_property(&self, prop_name: String) -> Result<String, PropertyError> {
        self.get_value(prop_name)
    }

    pub fn get_boolean_property(&self, prop_name: String) -> Result<bool, PropertyError> {
        parse_bool(self.get_value(prop_name)?)
    }

    pub fn get_uint32_property(&self, prop_name: String) -> Result<u32, PropertyError> {
        parse_uint32(self.get_value(prop_name)?)
    }

    pub fn get_uint64_property(&self, prop_name: String) -> Result<u64, PropertyError> {
        parse_uint64(self.get_value(prop_name)?)
    }

    pub fn get_binary_block_property(&self, prop_name: String) -> Result<Vec<u8>, PropertyError> {
        parse_binary_block(self.get_value(prop_name)?)
    }

    pub fn get_uuid_property(&self, prop_name: String) -> Result<TEE_UUID, PropertyError> {
        parse_uuid(self.get_value(prop_name)?)
    }

    pub fn get_identity_property(&self, prop_name: String) -> Result<TEE_Identity, PropertyError> {
        parse_identity(self.get_value(prop_name)?)
    }

    pub fn get_property_name_at_index(&self, index: usize) -> Result<String, PropertyError> {
        match self.properties.get_index(index) {
            Some((prop_name, (_, _))) => Ok(prop_name.to_string()),
            None => Err(PropertyError::ItemNotFound { name: format!("item at index {}", index) }),
        }
    }

    pub fn get_property_type_at_index(&self, index: usize) -> Result<PropType, PropertyError> {
        match self.properties.get_index(index) {
            Some((_, (prop_type, _))) => Ok(prop_type.clone()),
            None => Err(PropertyError::ItemNotFound { name: format!("item at index {}", index) }),
        }
    }
}

pub struct PropEnumerator {
    properties: Option<PropSet>,
    index: usize,
}

impl PropEnumerator {
    pub fn new() -> Self {
        Self { properties: None, index: 0 }
    }

    // Spec indicates that callers should start() before doing other operations.
    // This impl doesn't need to do any functional work here since the initial state is valid.
    pub fn start(&mut self, propset: PropSet) {
        self.properties = Some(propset);
        self.index = 0;
    }

    pub fn restart(&mut self) {
        self.index = 0;
    }

    pub fn next(&mut self) -> Result<(), PropertyError> {
        if self.properties.is_none() {
            return Err(PropertyError::ItemNotFound {
                name: "enumerator has not been started".to_string(),
            });
        }
        self.index = self.index + 1;
        Ok(())
    }

    pub fn get_property_name(&self) -> Result<String, PropertyError> {
        // This will return ItemNotFound when going out-of-bounds, compliant with spec.
        self.get_props()?.get_property_name_at_index(self.index)
    }

    pub fn get_property_type(&self) -> Result<PropType, PropertyError> {
        // This will return ItemNotFound when going out-of-bounds, compliant with spec.
        self.get_props()?.get_property_type_at_index(self.index)
    }

    pub fn get_property_as_string(&self) -> Result<String, PropertyError> {
        let prop_name = self.get_property_name()?;
        self.get_props()?.get_string_property(prop_name)
    }

    pub fn get_property_as_bool(&self) -> Result<bool, PropertyError> {
        let prop_name = self.get_property_name()?;
        self.get_props()?.get_boolean_property(prop_name)
    }

    pub fn get_property_as_u32(&self) -> Result<u32, PropertyError> {
        let prop_name = self.get_property_name()?;
        self.get_props()?.get_uint32_property(prop_name)
    }

    pub fn get_property_as_u64(&self) -> Result<u64, PropertyError> {
        let prop_name = self.get_property_name()?;
        self.get_props()?.get_uint64_property(prop_name)
    }

    pub fn get_property_as_binary_block(&self) -> Result<Vec<u8>, PropertyError> {
        let prop_name = self.get_property_name()?;
        self.get_props()?.get_binary_block_property(prop_name)
    }

    pub fn get_property_as_uuid(&self) -> Result<TEE_UUID, PropertyError> {
        let prop_name = self.get_property_name()?;
        self.get_props()?.get_uuid_property(prop_name)
    }

    pub fn get_property_as_identity(&self) -> Result<TEE_Identity, PropertyError> {
        let prop_name = self.get_property_name()?;
        self.get_props()?.get_identity_property(prop_name)
    }

    fn get_props(&self) -> Result<&PropSet, PropertyError> {
        match &self.properties {
            Some(prop_set) => Ok(prop_set),
            None => Err(PropertyError::ItemNotFound {
                name: "enumerator has not been started".to_string(),
            }),
        }
    }
}

// These parsing functions define format of value strings expected in config files.
fn parse_bool(value: String) -> Result<bool, PropertyError> {
    match value.parse::<bool>() {
        Ok(val) => Ok(val),
        Err(_) => Err(PropertyError::BadFormat { prop_type: PropType::Boolean, value }),
    }
}

fn parse_uint32(value: String) -> Result<u32, PropertyError> {
    match value.parse::<u32>() {
        Ok(val) => Ok(val),
        Err(_) => Err(PropertyError::BadFormat { prop_type: PropType::UnsignedInt32, value }),
    }
}

fn parse_uint64(value: String) -> Result<u64, PropertyError> {
    match value.parse::<u64>() {
        Ok(val) => Ok(val),
        Err(_) => Err(PropertyError::BadFormat { prop_type: PropType::UnsignedInt64, value }),
    }
}

fn parse_binary_block(value: String) -> Result<Vec<u8>, PropertyError> {
    // The string is expected to be base64 encoded.
    match STANDARD.decode(value.clone()) {
        Ok(bytes) => Ok(Vec::from(bytes)),
        Err(_) => Err(PropertyError::BadFormat { prop_type: PropType::BinaryBlock, value }),
    }
}

fn parse_uuid(value: String) -> Result<TEE_UUID, PropertyError> {
    match Uuid::parse_str(&value) {
        Ok(uuid) => {
            let (time_low, time_mid, time_hi_and_version, clock_seq_and_node) = uuid.as_fields();
            Ok(TEE_UUID {
                timeLow: time_low,
                timeMid: time_mid,
                timeHiAndVersion: time_hi_and_version,
                clockSeqAndNode: *clock_seq_and_node,
            })
        }
        Err(_) => Err(PropertyError::BadFormat { prop_type: PropType::Uuid, value }),
    }
}

fn parse_identity(value: String) -> Result<TEE_Identity, PropertyError> {
    let res: Result<TeeIdentity, serde_json5::Error> = serde_json5::from_str(&value);
    match res {
        Ok(identity) => {
            let tee_uuid = parse_uuid(identity.uuid)?;
            Ok(TEE_Identity { login: identity.login_type.to_u32(), uuid: tee_uuid })
        }
        Err(_) => Err(PropertyError::BadFormat { prop_type: PropType::Identity, value }),
    }
}

// TODO(b/366015756): Parsing logic should be fuzzed for production-hardening.
#[cfg(test)]
pub mod tests {
    use super::*;

    const TEST_PROP_NAME_STRING: &str = "gpd.tee.test.string";
    const TEST_PROP_NAME_BOOL: &str = "gpd.tee.test.bool";
    const TEST_PROP_NAME_U32: &str = "gpd.tee.test.u32";
    const TEST_PROP_NAME_U64: &str = "gpd.tee.test.u64";
    const TEST_PROP_NAME_BINARY_BLOCK: &str = "gpd.tee.test.binaryBlock";
    const TEST_PROP_NAME_UUID: &str = "gpd.tee.test.uuid";
    const TEST_PROP_NAME_IDENTITY: &str = "gpd.tee.test.identity";

    const TEST_PROP_VAL_STRING: &str = "asdf";
    const TEST_PROP_VAL_BOOL: &str = "true";
    const TEST_PROP_VAL_U32: &str = "57";
    const TEST_PROP_VAL_U64: &str = "4294967296"; // U32::MAX + 1
    const TEST_PROP_VAL_BINARY_BLOCK: &str = "ZnVjaHNpYQ=="; // base64 encoding of "fuchsia"
    const TEST_PROP_VAL_UUID: &str = "5b9e0e40-2636-11e1-ad9e-0002a5d5c51b"; // OS Test TA UUID

    fn create_test_tee_identity() -> TeeIdentity {
        TeeIdentity { login_type: LoginType::TrustedApp, uuid: TEST_PROP_VAL_UUID.to_string() }
    }

    fn create_test_tee_identity_string() -> String {
        let identity = create_test_tee_identity();
        serde_json5::to_string(&identity).expect("serializing json")
    }

    fn create_test_prop_map() -> PropertiesMap {
        let mut props: IndexMap<String, (PropType, String)> = IndexMap::new();
        props.insert(
            TEST_PROP_NAME_STRING.to_string(),
            (PropType::String, TEST_PROP_VAL_STRING.to_string()),
        );
        props.insert(
            TEST_PROP_NAME_BOOL.to_string(),
            (PropType::Boolean, TEST_PROP_VAL_BOOL.to_string()),
        );
        props.insert(
            TEST_PROP_NAME_U32.to_string(),
            (PropType::UnsignedInt32, TEST_PROP_VAL_U32.to_string()),
        );
        props.insert(
            TEST_PROP_NAME_U64.to_string(),
            (PropType::UnsignedInt64, TEST_PROP_VAL_U64.to_string()),
        );
        props.insert(
            TEST_PROP_NAME_BINARY_BLOCK.to_string(),
            (PropType::BinaryBlock, TEST_PROP_VAL_BINARY_BLOCK.to_string()),
        );
        props.insert(
            TEST_PROP_NAME_UUID.to_string(),
            (PropType::Uuid, TEST_PROP_VAL_UUID.to_string()),
        );
        props.insert(
            TEST_PROP_NAME_IDENTITY.to_string(),
            (PropType::Identity, create_test_tee_identity_string()),
        );
        props
    }

    fn create_test_prop_set() -> PropSet {
        PropSet::new(PropSetType::TeeImplementation, create_test_prop_map())
    }

    #[test]
    pub fn test_load_config_from_string() {
        let config_json = r#"[
            {
                "name": "gpd.tee.asdf",
                "prop_type": "boolean",
                "value": "true"
            },
            {
                "name": "gpd.tee.other",
                "prop_type": "binary_block",
                "value": "testingzz"
            }
        ]
        "#;

        let prop_set: PropSet =
            PropSet::from_config_string(config_json, PropSetType::TeeImplementation)
                .expect("loading config");

        let bool_prop_value =
            prop_set.get_boolean_property("gpd.tee.asdf".to_string()).expect("getting bool prop");
        assert_eq!(true, bool_prop_value)
    }

    #[test]
    pub fn test_load_config_from_string_failure() {
        // Invalid json (missing opening `{` for first object)
        let config_json = r#"[
                "name": "gpd.tee.asdf",
                "prop_type": "boolean",
                "value": "true"
            },
            {
                "name": "gpd.tee.other",
                "prop_type": "binary_block",
                "value": "testingzz"
            }
        ]
        "#;

        let res = PropSet::from_config_string(config_json, PropSetType::TeeImplementation);

        match res.err() {
            Some(PropertyError::Generic { .. }) => (),
            _ => assert!(false, "Unexpected error type"),
        }
    }

    // *** Enumerator Tests ***
    #[test]
    pub fn test_enumerator_query_prop_types_success() {
        let mut enumerator = PropEnumerator::new();

        // Enumerate in the order of test prop set, starting with string.
        enumerator.start(create_test_prop_set());

        let prop_name = enumerator.get_property_name().expect("getting prop name");
        assert_eq!(TEST_PROP_NAME_STRING.to_string(), prop_name);

        let prop_type = enumerator.get_property_type().expect("getting prop type");
        assert_eq!(PropType::String, prop_type);

        let prop_val = enumerator.get_property_as_string().expect("getting prop as string");
        assert_eq!(TEST_PROP_VAL_STRING.to_string(), prop_val);

        // bool.
        enumerator.next().expect("moving enumerator to next prop");

        let prop_name = enumerator.get_property_name().expect("getting prop name");
        assert_eq!(TEST_PROP_NAME_BOOL.to_string(), prop_name);

        let prop_type = enumerator.get_property_type().expect("getting prop type");
        assert_eq!(PropType::Boolean, prop_type);

        let prop_val = enumerator.get_property_as_bool().expect("getting prop as bool");
        assert_eq!(true, prop_val);

        // u32.
        enumerator.next().expect("moving enumerator to next prop");

        let prop_name = enumerator.get_property_name().expect("getting prop name");
        assert_eq!(TEST_PROP_NAME_U32.to_string(), prop_name);

        let prop_type = enumerator.get_property_type().expect("getting prop type");
        assert_eq!(PropType::UnsignedInt32, prop_type);

        let prop_val = enumerator.get_property_as_u32().expect("getting prop as u32");
        assert_eq!(57, prop_val);

        // u64.
        enumerator.next().expect("moving enumerator to next prop");

        let prop_name = enumerator.get_property_name().expect("getting prop name");
        assert_eq!(TEST_PROP_NAME_U64.to_string(), prop_name);

        let prop_type = enumerator.get_property_type().expect("getting prop type");
        assert_eq!(PropType::UnsignedInt64, prop_type);

        let prop_val = enumerator.get_property_as_u64().expect("getting prop as u64");
        assert_eq!(4294967296, prop_val);

        // Binary block.
        enumerator.next().expect("moving enumerator to next prop");

        let prop_name = enumerator.get_property_name().expect("getting prop name");
        assert_eq!(TEST_PROP_NAME_BINARY_BLOCK.to_string(), prop_name);

        let prop_type = enumerator.get_property_type().expect("getting prop type");
        assert_eq!(PropType::BinaryBlock, prop_type);

        let prop_val =
            enumerator.get_property_as_binary_block().expect("getting prop as binary block");
        let bytes_expected = STANDARD.decode("ZnVjaHNpYQ==").expect("decoding binary block string");
        assert_eq!(bytes_expected, prop_val);

        // UUID.
        enumerator.next().expect("moving enumerator to next prop");

        let prop_name = enumerator.get_property_name().expect("getting prop name");
        assert_eq!(TEST_PROP_NAME_UUID.to_string(), prop_name);

        let prop_type = enumerator.get_property_type().expect("getting prop type");
        assert_eq!(PropType::Uuid, prop_type);

        let prop_val: TEE_UUID = enumerator.get_property_as_uuid().expect("getting prop as uuid");
        let uuid_expected =
            Uuid::parse_str("5b9e0e40-2636-11e1-ad9e-0002a5d5c51b").expect("parsing uuid str");
        let (time_low, time_mid, time_hi_and_version, clock_seq_and_node) =
            uuid_expected.as_fields();
        assert_eq!(time_low, prop_val.timeLow);
        assert_eq!(time_mid, prop_val.timeMid);
        assert_eq!(time_hi_and_version, prop_val.timeHiAndVersion);
        assert_eq!(*clock_seq_and_node, prop_val.clockSeqAndNode);

        // Identity.
        enumerator.next().expect("moving enumerator to next prop");

        let prop_name = enumerator.get_property_name().expect("getting prop name");
        assert_eq!(TEST_PROP_NAME_IDENTITY.to_string(), prop_name);

        let prop_type = enumerator.get_property_type().expect("getting prop type");
        assert_eq!(PropType::Identity, prop_type);

        let prop_val: TEE_Identity =
            enumerator.get_property_as_identity().expect("getting prop as identity");
        let identity_expected = create_test_tee_identity();
        assert_eq!(identity_expected.login_type.to_u32(), prop_val.login);
        // This should have the same test UUID; can reuse parsed expected uuid fields.
        assert_eq!(time_low, prop_val.uuid.timeLow);
        assert_eq!(time_mid, prop_val.uuid.timeMid);
        assert_eq!(time_hi_and_version, prop_val.uuid.timeHiAndVersion);
        assert_eq!(*clock_seq_and_node, prop_val.uuid.clockSeqAndNode);

        // Test error upon going out of bounds and attempting to query next property.
        enumerator.next().expect("moving enumerator to next prop");

        let res = enumerator.get_property_name();
        match res.err() {
            Some(PropertyError::ItemNotFound { .. }) => (),
            _ => assert!(false, "Unexpected error type"),
        }
    }

    #[test]
    pub fn test_enumerator_restart() {
        let mut enumerator = PropEnumerator::new();
        enumerator.start(create_test_prop_set());

        let prop_name = enumerator.get_property_name().expect("getting prop name");
        assert_eq!(TEST_PROP_NAME_STRING.to_string(), prop_name);

        enumerator.next().expect("moving enumerator to next prop");

        let prop_name = enumerator.get_property_name().expect("getting prop name");
        assert_eq!(TEST_PROP_NAME_BOOL.to_string(), prop_name);

        enumerator.restart();

        let prop_name = enumerator.get_property_name().expect("getting prop name");
        assert_eq!(TEST_PROP_NAME_STRING.to_string(), prop_name);
    }

    #[test]
    pub fn test_enumerator_wrong_prop_type_error() {
        let mut enumerator = PropEnumerator::new();
        enumerator.start(create_test_prop_set());

        // First value is string type and not interpretable as bool, should error.
        let res = enumerator.get_property_as_bool();
        match res.err() {
            Some(PropertyError::BadFormat { .. }) => (),
            _ => assert!(false, "Unexpected error type"),
        }

        enumerator.next().expect("moving enumerator to next prop");

        // Second value is bool, not interpretable as identity, should error.
        let res = enumerator.get_property_as_identity();
        match res.err() {
            Some(PropertyError::BadFormat { .. }) => (),
            _ => assert!(false, "Unexpected error type"),
        }

        // Getting the same bool value as bool should still work.
        let res = enumerator.get_property_as_bool();
        assert!(res.is_ok());
    }

    #[test]
    pub fn test_enumerator_not_started_error() {
        // This enumerator functionally starts in a usable initial state, with internal index = 0.
        // The spec says callers should call start() first though, so we error if it isn't called.
        let enumerator = PropEnumerator::new();

        let res = enumerator.get_property_name();

        // Return code should map to item not found error per spec.
        match res.err() {
            Some(PropertyError::ItemNotFound { .. }) => (),
            _ => assert!(false, "Unexpected error type"),
        }
    }

    // *** PropSet Getter Tests ***
    #[test]
    pub fn test_propset_get_not_found() {
        let prop_set = create_test_prop_set();

        let res = prop_set.get_boolean_property("name.that.isnt.there".to_string());

        match res.err() {
            Some(PropertyError::ItemNotFound { .. }) => (),
            _ => assert!(false, "Unexpected error type"),
        }
    }

    #[test]
    pub fn test_propset_get_string_success() {
        let prop_set = create_test_prop_set();

        let test_str_val = prop_set
            .get_string_property(TEST_PROP_NAME_STRING.to_string())
            .expect("getting str prop val");
        assert_eq!(TEST_PROP_VAL_STRING.to_string(), test_str_val);

        // Spec indicates that any value can be represented as string, even if it doesn't match the type.
        let test_bool_val_as_str = prop_set
            .get_string_property(TEST_PROP_NAME_BOOL.to_string())
            .expect("getting bool prop val as string");
        assert_eq!(TEST_PROP_VAL_BOOL.to_string(), test_bool_val_as_str);
    }

    #[test]
    pub fn test_propset_get_bool_success() {
        let prop_set = create_test_prop_set();

        let val = prop_set
            .get_boolean_property(TEST_PROP_NAME_BOOL.to_string())
            .expect("getting bool prop val");
        assert_eq!(true, val);
    }

    #[test]
    pub fn test_propset_get_bool_wrong_type() {
        let prop_set = create_test_prop_set();

        let res = prop_set.get_boolean_property(TEST_PROP_NAME_UUID.to_string());
        match res.err() {
            Some(PropertyError::BadFormat { .. }) => (),
            _ => assert!(false, "Unexpected error type"),
        }
    }

    #[test]
    pub fn test_propset_get_u32_success() {
        let prop_set = create_test_prop_set();

        let val = prop_set
            .get_uint32_property(TEST_PROP_NAME_U32.to_string())
            .expect("getting u32 prop val");
        assert_eq!(57, val);
    }

    #[test]
    pub fn test_propset_get_u32_wrong_type() {
        let prop_set = create_test_prop_set();

        let res = prop_set.get_uint32_property(TEST_PROP_NAME_BOOL.to_string());
        match res.err() {
            Some(PropertyError::BadFormat { .. }) => (),
            _ => assert!(false, "Unexpected error type"),
        }
    }

    #[test]
    pub fn test_propset_get_u64_success() {
        let prop_set = create_test_prop_set();

        let val = prop_set
            .get_uint64_property(TEST_PROP_NAME_U64.to_string())
            .expect("getting u64 prop val");
        assert_eq!(4294967296, val);
    }

    #[test]
    pub fn test_propset_get_u64_wrong_type() {
        let prop_set = create_test_prop_set();

        let res = prop_set.get_uint64_property(TEST_PROP_NAME_BOOL.to_string());
        match res.err() {
            Some(PropertyError::BadFormat { .. }) => (),
            _ => assert!(false, "Unexpected error type"),
        }
    }

    #[test]
    pub fn test_propset_get_binary_block_success() {
        let prop_set = create_test_prop_set();

        let val = prop_set
            .get_binary_block_property(TEST_PROP_NAME_BINARY_BLOCK.to_string())
            .expect("getting binary block prop val");

        let expected_bytes = STANDARD
            .decode(TEST_PROP_VAL_BINARY_BLOCK)
            .expect("decoding expected binary block bytes");
        assert_eq!(expected_bytes, val);
    }

    #[test]
    pub fn test_propset_get_binary_block_wrong_type() {
        let prop_set = create_test_prop_set();

        // Technically most of the test values (bool, u32, u64, string) are valid base64 strings.
        // Use the serialized identity string to trigger parse failure since it has invalid chars.
        let res = prop_set.get_binary_block_property(TEST_PROP_NAME_IDENTITY.to_string());
        match res.err() {
            Some(PropertyError::BadFormat { .. }) => (),
            _ => assert!(false, "Unexpected error type"),
        }
    }

    #[test]
    pub fn test_propset_get_uuid_success() {
        let prop_set = create_test_prop_set();

        let val = prop_set
            .get_uuid_property(TEST_PROP_NAME_UUID.to_string())
            .expect("getting uuid prop val");

        let uuid_expected = Uuid::parse_str(TEST_PROP_VAL_UUID).expect("parsing uuid str");
        let (time_low, time_mid, time_hi_and_version, clock_seq_and_node) =
            uuid_expected.as_fields();
        assert_eq!(time_low, val.timeLow);
        assert_eq!(time_mid, val.timeMid);
        assert_eq!(time_hi_and_version, val.timeHiAndVersion);
        assert_eq!(*clock_seq_and_node, val.clockSeqAndNode);
    }

    #[test]
    pub fn test_propset_get_uuid_wrong_type() {
        let prop_set = create_test_prop_set();

        let res = prop_set.get_uuid_property(TEST_PROP_NAME_BOOL.to_string());
        match res.err() {
            Some(PropertyError::BadFormat { .. }) => (),
            _ => assert!(false, "Unexpected error type"),
        }
    }

    #[test]
    pub fn test_propset_get_identity_success() {
        let prop_set = create_test_prop_set();

        let val: TEE_Identity = prop_set
            .get_identity_property(TEST_PROP_NAME_IDENTITY.to_string())
            .expect("getting identity prop val");

        assert_eq!(LoginType::TrustedApp.to_u32(), val.login);

        let uuid_expected = Uuid::parse_str(TEST_PROP_VAL_UUID).expect("parsing uuid str");
        let (time_low, time_mid, time_hi_and_version, clock_seq_and_node) =
            uuid_expected.as_fields();
        assert_eq!(time_low, val.uuid.timeLow);
        assert_eq!(time_mid, val.uuid.timeMid);
        assert_eq!(time_hi_and_version, val.uuid.timeHiAndVersion);
        assert_eq!(*clock_seq_and_node, val.uuid.clockSeqAndNode);
    }

    #[test]
    pub fn test_propset_get_identity_wrong_type() {
        let prop_set = create_test_prop_set();

        let res = prop_set.get_identity_property(TEST_PROP_NAME_BOOL.to_string());
        match res.err() {
            Some(PropertyError::BadFormat { .. }) => (),
            _ => assert!(false, "Unexpected error type"),
        }
    }

    // *** Parsing Function Tests ***
    #[test]
    pub fn test_parse_bool_success() {
        let val_true = "true".to_string();
        let val_false = "false".to_string();

        let res_true = parse_bool(val_true).expect("parsing true");
        let res_false = parse_bool(val_false).expect("parsing false");

        assert!(res_true);
        assert!(!res_false);
    }

    #[test]
    pub fn test_parse_bool_empty_string() {
        let val = "".to_string();

        let res = parse_bool(val);

        match res.err() {
            Some(PropertyError::BadFormat { .. }) => (),
            _ => assert!(false, "Unexpected error type"),
        }
    }

    #[test]
    pub fn test_parse_bool_bad_format() {
        let val = "asdf".to_string();

        let res = parse_bool(val);

        match res.err() {
            Some(PropertyError::BadFormat { .. }) => (),
            _ => assert!(false, "Unexpected error type"),
        }
    }

    #[test]
    pub fn test_parse_bool_caps_not_accepted() {
        let val = "TRUE".to_string();

        let res = parse_bool(val);

        match res.err() {
            Some(PropertyError::BadFormat { .. }) => (),
            _ => assert!(false, "Unexpected error type"),
        }
    }

    #[test]
    pub fn test_parse_u32_success() {
        let val = "15".to_string();

        let res = parse_uint32(val).expect("parsing 15");

        assert_eq!(res, 15);
    }

    #[test]
    pub fn test_parse_u32_empty_string() {
        let val = "".to_string();

        let res = parse_uint32(val);

        match res.err() {
            Some(PropertyError::BadFormat { .. }) => (),
            _ => assert!(false, "Unexpected error type"),
        }
    }

    #[test]
    pub fn test_parse_u32_negative_value() {
        let val = "-15".to_string();

        let res = parse_uint32(val);

        match res.err() {
            Some(PropertyError::BadFormat { .. }) => (),
            _ => assert!(false, "Unexpected error type"),
        }
    }

    #[test]
    pub fn test_parse_u32_too_large() {
        // u32::MAX = 4_294_967_295u32
        let val = "4294967296".to_string();

        let res = parse_uint32(val);

        match res.err() {
            Some(PropertyError::BadFormat { .. }) => (),
            _ => assert!(false, "Unexpected error type"),
        }
    }

    #[test]
    pub fn test_parse_u32_bad_format() {
        let val = "text".to_string();

        let res = parse_uint32(val);

        match res.err() {
            Some(PropertyError::BadFormat { .. }) => (),
            _ => assert!(false, "Unexpected error type"),
        }
    }

    #[test]
    pub fn test_parse_u64_success() {
        let val = "4294967296".to_string();

        let res = parse_uint64(val).expect("parsing 4294967296");

        assert_eq!(res, 4294967296);
    }

    #[test]
    pub fn test_parse_u64_empty_string() {
        let val = "".to_string();

        let res = parse_uint64(val);

        match res.err() {
            Some(PropertyError::BadFormat { .. }) => (),
            _ => assert!(false, "Unexpected error type"),
        }
    }

    #[test]
    pub fn test_parse_u64_negative_value() {
        let val = "-15".to_string();

        let res = parse_uint64(val);

        match res.err() {
            Some(PropertyError::BadFormat { .. }) => (),
            _ => assert!(false, "Unexpected error type"),
        }
    }

    #[test]
    pub fn test_parse_u64_too_large() {
        // u64::MAX = 18_446_744_073_709_551_615u64
        let val = "18446744073709551616".to_string();

        let res = parse_uint64(val);

        match res.err() {
            Some(PropertyError::BadFormat { .. }) => (),
            _ => assert!(false, "Unexpected error type"),
        }
    }

    #[test]
    pub fn test_parse_u64_bad_format() {
        let val = "text".to_string();

        let res = parse_uint64(val);

        match res.err() {
            Some(PropertyError::BadFormat { .. }) => (),
            _ => assert!(false, "Unexpected error type"),
        }
    }

    #[test]
    pub fn test_parse_binary_block_success() {
        let bytes: [u8; 6] = [1, 2, 3, 4, 5, 6];
        let encoded = STANDARD.encode(bytes);

        let res = parse_binary_block(encoded).expect("parsing binary block");

        assert_eq!(bytes.to_vec(), res);
    }

    #[test]
    pub fn test_parse_binary_empty_string() {
        let res = parse_binary_block("".to_string());

        assert!(res.is_ok());
    }

    #[test]
    pub fn test_parse_binary_block_invalid_base64() {
        // Include characters outside of standard base64 set.
        let bad_val = "asdf&^%@".to_string();

        let res = parse_binary_block(bad_val);

        match res.err() {
            Some(PropertyError::BadFormat { .. }) => (),
            _ => assert!(false, "Unexpected error type"),
        }
    }

    #[test]
    pub fn test_parse_uuid_success() {
        let valid_uuid = "5b9e0e40-2636-11e1-ad9e-0002a5d5c51b".to_string();

        let res = parse_uuid(valid_uuid);

        assert!(res.is_ok());
    }

    #[test]
    pub fn test_parse_uuid_empty_string() {
        let res = parse_uuid("".to_string());

        match res.err() {
            Some(PropertyError::BadFormat { .. }) => (),
            _ => assert!(false, "Unexpected error type"),
        }
    }

    #[test]
    pub fn test_parse_uuid_invalid_uuid() {
        let invalid_uuid = "asdf".to_string();

        let res = parse_uuid(invalid_uuid);

        match res.err() {
            Some(PropertyError::BadFormat { .. }) => (),
            _ => assert!(false, "Unexpected error type"),
        }
    }

    #[test]
    pub fn test_parse_identity_success() {
        let valid_uuid = "5b9e0e40-2636-11e1-ad9e-0002a5d5c51b".to_string();
        let identity = TeeIdentity { login_type: LoginType::TrustedApp, uuid: valid_uuid };
        let json = serde_json5::to_string(&identity).expect("serializing json");

        let res = parse_identity(json);

        assert!(res.is_ok());
    }

    #[test]
    pub fn test_parse_identity_empty_uuid() {
        let identity = TeeIdentity { login_type: LoginType::TrustedApp, uuid: "".to_string() };
        let json = serde_json5::to_string(&identity).expect("serializing json");

        let res = parse_identity(json);

        match res.err() {
            Some(PropertyError::BadFormat { .. }) => (),
            _ => assert!(false, "Unexpected error type"),
        }
    }

    #[test]
    pub fn test_parse_identity_invalid_login_type() {
        let json = "{\"login_type\":\"SomeRandomValue\",\"uuid\":\"5b9e0e40-2636-11e1-ad9e-0002a5d5c51b\"}".to_string();

        let res = parse_identity(json);

        match res.err() {
            Some(PropertyError::BadFormat { .. }) => (),
            _ => assert!(false, "Unexpected error type"),
        }
    }
}
