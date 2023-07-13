// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Debug;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing;

const POST_EVENT_COUNT_MAX: usize = 25;
const EVENT_PARAM_COUNT_MAX: usize = 25;
const EVENT_NAME_LENGTH_MAX: usize = 40;

const EVENT_USER_PROPERTY_COUNT_MAX: usize = 25;
const PARAM_NAME_LENGTH_MAX: usize = 40;
const PARAM_VALUE_LENGTH_MAX: usize = 100;
const USER_PROPERTY_NAME_LENGTH_MAX: usize = 24;
const USER_PROPERTY_VALUE_LENGTH_MAX: usize = 36;
const ITEM_PARAM_COUNT_MAX: usize = 10;

/// These structs model the Measurement Protocol format of Google Analytics 4.
/// This is to provide easy validation and json serialization of POST data.
/// https://developers.google.com/analytics/devguides/collection/protocol/ga4
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Event {
    pub(crate) name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) params: Option<Params>,
    // TODO once serde_json supports deserializing u128, change type to u128
    pub(crate) timestamp_micros: String,
}

impl Event {
    pub fn new(name: String, params: Option<Params>) -> Self {
        Event { name, params, timestamp_micros: get_systime().to_string() }
    }

    pub(crate) fn add_param<T: Into<GA4Value>>(&mut self, param_key: &str, param_value: T) {
        match &mut self.params {
            Some(params) => params.add_param(param_key, param_value),
            None => {
                let mut new_params = Params::default();
                new_params.add_param(param_key, param_value.into());
                self.params = Some(new_params);
            }
        };
    }

    pub fn validate(&self) -> Result<(), anyhow::Error> {
        self.validate_name_length()?;
        self.validate_params()
    }

    pub fn validate_name_length(&self) -> Result<(), anyhow::Error> {
        validate_string_len(&self.name, EVENT_NAME_LENGTH_MAX)
    }

    fn validate_params(&self) -> std::result::Result<(), anyhow::Error> {
        if let Some(ps) = &self.params {
            ps.validate()
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Params {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) items: Option<Vec<HashMap<String, GA4Value>>>,
    #[serde(flatten)]
    pub(crate) params: HashMap<String, GA4Value>,
}

impl Params {
    fn add_param<'a, T: Into<GA4Value>>(&mut self, param_key: &str, param_value: T) {
        self.params.insert(param_key.into(), param_value.into());
    }

    fn validate(&self) -> Result<(), anyhow::Error> {
        if self.params.keys().count() > EVENT_PARAM_COUNT_MAX {
            bail!("Too many params in Event")
        }
        // TODO add name, value validations for self::items if we ever start using them.
        // Currently, we are not using them.
        if let Some(items) = &self.items {
            if items.len() > ITEM_PARAM_COUNT_MAX {
                bail!("Too many item params. Limit is {}", ITEM_PARAM_COUNT_MAX)
            }
        }
        self.validate_params()
    }

    fn validate_params(&self) -> std::result::Result<(), anyhow::Error> {
        validate_map(&self.params, PARAM_NAME_LENGTH_MAX, |v: &GA4Value| match v {
            GA4Value::Str(s) => s.len() > PARAM_VALUE_LENGTH_MAX,
            _ => false,
        })
    }
}

impl Default for Params {
    fn default() -> Self {
        Params { items: None, params: HashMap::new() }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct ValueObject {
    pub(crate) value: GA4Value,
}

impl ValueObject {
    fn validate(&self) -> Result<(), anyhow::Error> {
        match &self.value {
            GA4Value::Str(s) => {
                if s.len() > USER_PROPERTY_VALUE_LENGTH_MAX {
                    bail!(
                        "User property value {:?} is greater than max {:?}",
                        &self.value,
                        USER_PROPERTY_VALUE_LENGTH_MAX
                    )
                }
            }
            _ => (),
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
pub enum GA4Value {
    Float(f64),
    Integer(i64),
    UInteger(u64),
    Bool(bool),
    Str(String),
}

impl From<String> for GA4Value {
    fn from(s: String) -> Self {
        GA4Value::Str(s)
    }
}

impl From<&str> for GA4Value {
    fn from(s: &str) -> Self {
        GA4Value::Str(String::from(s))
    }
}

impl From<f64> for GA4Value {
    fn from(f: f64) -> Self {
        GA4Value::Float(f)
    }
}

impl From<i64> for GA4Value {
    fn from(i: i64) -> Self {
        GA4Value::Integer(i)
    }
}

impl From<u64> for GA4Value {
    fn from(u: u64) -> Self {
        GA4Value::UInteger(u)
    }
}

impl From<bool> for GA4Value {
    fn from(b: bool) -> Self {
        GA4Value::Bool(b)
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Post {
    pub(crate) client_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) user_id: Option<String>,
    pub(crate) non_personalized_ads: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) user_properties: Option<HashMap<String, ValueObject>>,
    pub(crate) events: Vec<Event>,
}

impl Post {
    pub(crate) fn new(
        client_id: String,
        user_id: Option<String>,
        user_properties: Option<HashMap<String, ValueObject>>,
        events: Vec<Event>,
    ) -> Self {
        Post { client_id, user_id, non_personalized_ads: true, user_properties, events }
    }

    pub(crate) fn add_event(&mut self, event: Event) {
        let evs = &mut self.events;
        evs.push(event);
    }

    pub(crate) fn to_json(&mut self) -> String {
        let json = serde_json::json!(self).to_string();
        let e = &mut self.events;
        e.clear(); // flush events in post
        json
    }

    pub fn validate(&self) -> Result<(), anyhow::Error> {
        if self.events.len() > POST_EVENT_COUNT_MAX {
            bail!("Too many events in Post. Limit is {}", POST_EVENT_COUNT_MAX);
        }
        if let Some(event) = self.events.iter().find(|e| e.validate().is_err()) {
            return event.validate(); // returns the error result for the found event
        }
        self.validate_user_properties()
    }

    fn validate_user_properties(&self) -> std::result::Result<(), anyhow::Error> {
        match &self.user_properties {
            Some(props) => {
                if props.keys().len() > EVENT_USER_PROPERTY_COUNT_MAX {
                    bail!("Too many user parameters. Limit is {}", EVENT_USER_PROPERTY_COUNT_MAX)
                }
                validate_map(props, USER_PROPERTY_NAME_LENGTH_MAX, |v: &ValueObject| {
                    v.validate().is_err()
                })
            }
            None => Ok(()),
        }
    }
}

impl Default for Post {
    fn default() -> Self {
        Post {
            client_id: "Unknown client id".to_string(),
            user_id: None,
            non_personalized_ads: true,
            user_properties: None,
            events: vec![],
        }
    }
}

/// Produces post body to send to the GA 4 analytics service
pub(crate) fn make_ga4_event<'a>(
    category: Option<&str>,
    action: Option<&str>,
    label: Option<&str>,
    custom_dimensions: impl IntoIterator<Item = (&'a str, GA4Value)>,
    invoker: Option<&str>,
    event_name: Option<&str>,
) -> Event {
    tracing::debug!(
        "Make GA4 ARGS: cat:{:?}, event:{:?}, action:{:?}, label:{:?}",
        category,
        event_name,
        action,
        label
    );

    let params: &mut HashMap<String, GA4Value> = &mut HashMap::new();
    if let Some(s) = invoker {
        params.insert("invoker".into(), s.clone().into());
    }
    insert_if_present("label", params, label);

    for (key, value) in custom_dimensions.into_iter() {
        params.insert(key.into(), value.clone().into());
    }

    insert_if_present("args", params, action);

    // for ga4 migration, if category name is 'general' then use 'invoke' for the event name.
    // if it is anthying else, use the category as the event name.
    // When UA is turned down and we migrate all client code, send event name explicitly
    // instead of category
    let event_name_string = match event_name {
        Some(name) => match name {
            "general" => "invoke".to_string(),
            _ => name.to_string(),
        },
        None => {
            if let Some(category_name) = category {
                category_name.to_string()
            } else {
                "invoke".to_string()
            }
        }
    };

    if params.is_empty() {
        Event::new(event_name_string, None)
    } else {
        Event::new(event_name_string, Some(Params { items: None, params: params.to_owned() }))
    }
}

// Creates a post body to send to GA4 analytics
// containing exception and crash information
pub(crate) fn make_ga4_crash_event(
    description: &str,
    fatal: Option<&bool>,
    invoker: Option<&str>,
) -> Event {
    let params: &mut HashMap<String, GA4Value> = &mut HashMap::new();
    if let Some(s) = invoker {
        params.insert("invoker".into(), s.clone().into());
    }
    insert_if_present(
        "fatal",
        params,
        match fatal {
            Some(true) => Some("1"),
            _ => Some("0"),
        },
    );
    params.insert("description".into(), description.into());

    Event::new("exception".into(), Some(Params { items: None, params: params.to_owned() }))
}

// Creates a post body to send to GA4 Analytics
// representing a timing event
pub(crate) fn make_ga4_timing_event<'a>(
    command: Option<&str>,
    time: u64,
    variable: Option<&str>,
    label: Option<&str>,
    custom_dimensions: impl IntoIterator<Item = (&'a str, GA4Value)>,
    invoker: Option<&str>,
) -> Event {
    let params: &mut HashMap<String, GA4Value> = &mut HashMap::new();
    if let Some(value) = command {
        params.insert("command".into(), value.into());
    }
    params.insert("time".into(), time.into());
    insert_if_present("variable", params, variable);
    insert_if_present("label", params, label);
    for (key, value) in custom_dimensions.into_iter() {
        params.insert(key.into(), value.clone());
    }
    if let Some(s) = invoker {
        params.insert("invoker".into(), s.clone().into());
    }
    Event::new("timing".into(), Some(Params { items: None, params: params.to_owned() }))
}

pub fn validate_string_len(string: &str, max_len: usize) -> Result<(), anyhow::Error> {
    match string.len() <= max_len {
        true => Ok(()),
        false => bail!("String, {}, longer than max length {}", string, max_len),
    }
}

/// Ensure that the keys and values of a map
/// adhere to the Measurement Protocol constraints.
/// The value_predicate parameter allows differing tests on values.
fn validate_map<'a, F: Fn(&V) -> bool, V: Debug>(
    hash_map: &'a HashMap<String, V>,
    key_max_length: usize,
    value_error_predicate: F,
) -> std::result::Result<(), anyhow::Error> {
    for (k, v) in hash_map {
        if k.len() > key_max_length {
            bail!("Key too long: {:?}", k);
        }
        if value_error_predicate(v) {
            bail!("Value too long: {:?}", v);
        }
    }
    Ok(())
}

// If value is present, add to params.
pub(crate) fn insert_if_present(
    key: &str,
    params: &mut HashMap<String, GA4Value>,
    value: Option<&str>,
) {
    match value {
        Some(val) => {
            params.insert(key.to_owned(), val.into());
        }
        None => (),
    };
}

fn get_systime() -> u128 {
    let start = SystemTime::now();
    let since = start.duration_since(UNIX_EPOCH).expect("Time went backwards");
    since.as_micros()
}

/// Converts the old, UA, custom dimensions to the new, GA4, GA4Value type.
/// Currently, to preserve existing clients, everything will become GA4Value::Str.
/// This will go away once we remove UA analytics and refactor the clients to send specific types.
pub fn convert_to_ga4values(custom_dimensions: BTreeMap<&str, String>) -> BTreeMap<&str, GA4Value> {
    let custom_dimensions_ga4: &mut BTreeMap<&str, GA4Value> = &mut BTreeMap::new();
    for (k, v) in custom_dimensions.iter() {
        custom_dimensions_ga4.insert(*k, v.clone().into());
    }
    custom_dimensions_ga4.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_param_serde_flattening() {
        let item_map: HashMap<String, GA4Value> = HashMap::from([
            ("item1".to_string(), "value1".into()),
            ("item2".to_string(), "value2".into()),
        ]);
        let param_map: HashMap<String, GA4Value> = HashMap::from([
            ("param1".to_string(), "value1".into()),
            ("param2".to_string(), "value2".into()),
        ]);
        let p = Params { items: Some(vec![item_map]), params: param_map };
        let result = serde_json::json!(p);
        let expected_json = "{\"items\":[{\"item1\":\"value1\",\"item2\":\"value2\"}],\"param1\":\"value1\",\"param2\":\"value2\"}";
        assert_eq!(result.to_string(), expected_json);
    }

    #[test]
    fn test_insert_if_present_some() {
        let map: &mut HashMap<String, GA4Value> = &mut HashMap::new();
        assert_eq!(map.len(), 0);
        insert_if_present("newkey", map, Some("newvalue"));
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn test_insert_if_present_none() {
        let map: &mut HashMap<String, GA4Value> = &mut HashMap::new();
        assert_eq!(map.len(), 0);
        insert_if_present("newkey", map, None);
        assert_eq!(map.len(), 0);
    }

    #[test]
    fn validate_str_len_too_long() {
        let result = validate_string_len(&String::from("foo"), 2);
        assert!(result.is_err());
    }

    #[test]
    fn validate_str_len_not_too_long() {
        let result = validate_string_len(&String::from("foo"), 3);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_keys_none() {
        let map: &mut HashMap<String, GA4Value> = &mut HashMap::new();
        assert!(validate_map(&map, 10, |v: &GA4Value| match v {
            GA4Value::Str(s) => {
                s.len() > 10
            }
            _ => false,
        })
        .is_ok());
    }

    #[test]
    fn validate_keys_one_too_long() {
        let map: &mut HashMap<String, GA4Value> =
            &mut HashMap::from([("key_too_long".to_string(), GA4Value::Str("value".to_string()))]);
        assert!(validate_map(&map, 10, |v: &GA4Value| match v {
            GA4Value::Str(s) => {
                s.len() > 10
            }
            _ => false,
        })
        .is_err());
    }

    #[test]
    fn validate_keys_one_ok() {
        let map =
            &mut HashMap::from([("key_too_long".to_string(), GA4Value::Str("value".to_string()))]);
        assert!(validate_map(&map, 12, |v: &GA4Value| match v {
            GA4Value::Str(s) => {
                s.len() > 10
            }
            _ => false,
        })
        .is_ok());
    }

    #[test]
    fn validate_values_none() {
        let map: &mut HashMap<String, GA4Value> = &mut HashMap::new();
        let result = validate_map(&map, PARAM_NAME_LENGTH_MAX, |v: &GA4Value| match v {
            GA4Value::Str(s) => s.chars().count() > PARAM_VALUE_LENGTH_MAX,
            _ => false,
        });
        assert!(result.is_ok());
    }

    #[test]
    fn validate_values_one_good() {
        let map: &mut HashMap<String, GA4Value> =
            &mut HashMap::from([("ok_key".to_string(), GA4Value::Str("ok_value".to_string()))]);
        let result = validate_map(&map, PARAM_NAME_LENGTH_MAX, |v: &GA4Value| match v {
            GA4Value::Str(s) => s.chars().count() > PARAM_VALUE_LENGTH_MAX,
            _ => false,
        });
        assert!(result.is_ok());
    }

    #[test]
    fn validate_values_one_too_long() {
        let map: &mut HashMap<String, GA4Value> =
            &mut HashMap::from([("ok_key".to_string(), GA4Value::Str("12345678901".to_string()))]);
        let result = validate_map(&map, PARAM_NAME_LENGTH_MAX, |v: &GA4Value| match v {
            GA4Value::Str(s) => s.chars().count() > 10,
            _ => false,
        });
        assert!(result.is_err());
    }

    #[test]
    fn convert_to_ga4value_types() {
        let custom_dimensions: &mut BTreeMap<&str, String> = &mut BTreeMap::new();
        custom_dimensions.insert("bool", "True".to_string());
        custom_dimensions.insert("i64", "1i64".to_string());
        custom_dimensions.insert("str", "hello".to_string());
        let actual = convert_to_ga4values(custom_dimensions.to_owned());
        assert_eq!(actual.get("bool"), Some(&GA4Value::Str("True".to_string())));
        assert_eq!(actual.get("i64"), Some(&GA4Value::Str("1i64".to_string())));
        assert_eq!(actual.get("str"), Some(&GA4Value::Str("hello".to_string())));
    }

    #[test]
    fn valueobject_validation_ok() {
        let v = ValueObject { value: GA4Value::Str("value for event of interest".to_string()) };
        assert!(v.validate().is_ok());
    }

    #[test]
    fn valueobject_validation_too_long() {
        let v = ValueObject {
            value: GA4Value::Str("01234567890123456789012345678901234567".to_string()),
        };
        assert!(v.validate().is_err());
    }

    #[test]
    fn make_post_event() {
        let args = "config analytics enable";
        let expected = String::from("{\"client_id\":\"1\",\"events\":[{\"name\":\"invoke\",\"params\":{\"label\":\"config analytics enable\"},\"timestamp_micros\":\"1\"}],\"non_personalized_ads\":true}");
        let mut event =
            make_ga4_event(None, None, Some(args), BTreeMap::new(), None, Some("invoke"));
        event.timestamp_micros = 1.to_string(); // timestamps need to be set to the same time for strings to match
        let post = &mut Post::new("1".to_string(), None, None, vec![event]);

        assert_eq!(expected, post.to_json());
    }
}
