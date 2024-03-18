// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use pest::{iterators::Pair, Parser};

use fidl_fuchsia_hardware_network as fhnet;
use fidl_fuchsia_net_filter_ext as filter_ext;

use crate::grammar::{Error, FilterRuleParser, Rule};

fn parse_action(pair: Pair<'_, Rule>) -> filter_ext::Action {
    assert_eq!(pair.as_rule(), Rule::action);
    match pair.into_inner().next().unwrap().as_rule() {
        Rule::pass => filter_ext::Action::Accept,
        Rule::drop => filter_ext::Action::Drop,
        // TODO(https://fxbug.dev/329500057): Remove dropreset from the grammar
        // as it is unimplemented in filter.deprecated and currently unsupported
        // in the filter2 API
        Rule::dropreset => todo!("not yet supported in the filter2 API"),
        _ => unreachable!("action must be one of (pass|drop|dropreset)"),
    }
}

// A subset of `filter_ext::IpHook` to handle current
// parsing capabilities.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Direction {
    LocalIngress,
    LocalEgress,
}

fn parse_direction(pair: Pair<'_, Rule>) -> Direction {
    assert_eq!(pair.as_rule(), Rule::direction);
    match pair.into_inner().next().unwrap().as_rule() {
        Rule::incoming => Direction::LocalIngress,
        Rule::outgoing => Direction::LocalEgress,
        _ => unreachable!("direction must be one of (in|out)"),
    }
}

// A subset of `filter_ext::TransportProtocolMatcher` to handle current
// parsing capabilities.
enum TransportProtocol {
    Tcp,
    Udp,
    Icmp,
}

fn parse_proto(pair: Pair<'_, Rule>) -> Option<TransportProtocol> {
    assert_eq!(pair.as_rule(), Rule::proto);
    pair.into_inner().next().map(|pair| match pair.as_rule() {
        Rule::tcp => TransportProtocol::Tcp,
        Rule::udp => TransportProtocol::Udp,
        Rule::icmp => TransportProtocol::Icmp,
        _ => unreachable!("protocol must be one of (tcp|udp|icmp)"),
    })
}

fn parse_devclass(pair: Pair<'_, Rule>) -> Option<filter_ext::DeviceClass> {
    assert_eq!(pair.as_rule(), Rule::devclass);
    pair.into_inner().next().map(|pair| match pair.as_rule() {
        Rule::virt => filter_ext::DeviceClass::Device(fhnet::DeviceClass::Virtual.into()),
        Rule::ethernet => filter_ext::DeviceClass::Device(fhnet::DeviceClass::Ethernet.into()),
        Rule::wlan => filter_ext::DeviceClass::Device(fhnet::DeviceClass::Wlan.into()),
        Rule::ppp => filter_ext::DeviceClass::Device(fhnet::DeviceClass::Ppp.into()),
        Rule::bridge => filter_ext::DeviceClass::Device(fhnet::DeviceClass::Bridge.into()),
        Rule::ap => filter_ext::DeviceClass::Device(fhnet::DeviceClass::WlanAp.into()),
        _ => unreachable!("devclass must be one of (virt|ethernet|wlan|ppp|bridge|ap)"),
    })
}

fn parse_rule(
    pair: Pair<'_, Rule>,
    routines: &FilterRoutines,
    index: usize,
) -> Result<filter_ext::Rule, Error> {
    assert_eq!(pair.as_rule(), Rule::rule);
    let mut pairs = pair.into_inner();

    let action = parse_action(pairs.next().unwrap());
    let direction = parse_direction(pairs.next().unwrap());
    let proto = parse_proto(pairs.next().unwrap());
    let device_class = parse_devclass(pairs.next().unwrap());
    let mut in_interface = None;
    let mut out_interface = None;
    let routine_id = match direction {
        Direction::LocalIngress => {
            // Use the same RoutineId as the LocalIngress routine.
            let Some(ref routine_id) = routines.local_ingress else {
                return Err(Error::RoutineNotProvided(direction));
            };
            in_interface =
                device_class.map(|class| filter_ext::InterfaceMatcher::DeviceClass(class));
            routine_id
        }
        Direction::LocalEgress => {
            // Use the same RoutineId as the LocalEgress routine.
            let Some(ref routine_id) = routines.local_egress else {
                return Err(Error::RoutineNotProvided(direction));
            };
            out_interface =
                device_class.map(|class| filter_ext::InterfaceMatcher::DeviceClass(class));
            routine_id
        }
    };
    // TODO(antoniolinhart): Once port numbers are parsed, use them for the
    // src_port/dst_port in the TransportProtocol.
    let transport_protocol = proto.map(|proto| match proto {
        TransportProtocol::Tcp => {
            filter_ext::TransportProtocolMatcher::Tcp { src_port: None, dst_port: None }
        }
        TransportProtocol::Udp => {
            filter_ext::TransportProtocolMatcher::Udp { src_port: None, dst_port: None }
        }
        TransportProtocol::Icmp => filter_ext::TransportProtocolMatcher::Icmp,
    });

    Ok(filter_ext::Rule {
        id: filter_ext::RuleId { routine: routine_id.clone(), index: index as u32 },
        matchers: filter_ext::Matchers {
            in_interface,
            out_interface,
            transport_protocol,
            ..Default::default()
        },
        action,
    })
}

// A container for `filter_ext::Routine`s that back the
// `filter_ext::IpInstallationHook`s currently supported
// by the parser.
#[derive(Debug, Default)]
pub struct FilterRoutines {
    pub local_ingress: Option<filter_ext::RoutineId>,
    pub local_egress: Option<filter_ext::RoutineId>,
}

pub fn parse_str_to_rules(
    line: &str,
    routines: &FilterRoutines,
) -> Result<Vec<filter_ext::Rule>, Error> {
    let mut pairs = FilterRuleParser::parse(Rule::rules, &line).map_err(Error::Pest)?;
    let mut rules = Vec::new();
    for (index, filter_rule) in pairs.next().unwrap().into_inner().into_iter().enumerate() {
        match filter_rule.as_rule() {
            Rule::rule => {
                let rule = parse_rule(filter_rule, &routines, index)?;
                rules.push(rule);
            }
            Rule::EOI => (),
            _ => unreachable!("rule must only have a rule case"),
        }
    }
    Ok(rules)
}

#[cfg(test)]
mod test {
    use super::*;

    fn test_filter_routines() -> FilterRoutines {
        FilterRoutines {
            local_ingress: Some(local_ingress_routine()),
            local_egress: Some(local_egress_routine()),
        }
    }

    fn local_ingress_routine() -> filter_ext::RoutineId {
        test_routine_id("local_ingress")
    }

    fn local_egress_routine() -> filter_ext::RoutineId {
        test_routine_id("local_egress")
    }

    fn test_routine_id(name: &str) -> filter_ext::RoutineId {
        filter_ext::RoutineId {
            namespace: filter_ext::NamespaceId(String::from("namespace")),
            name: String::from(name),
        }
    }

    #[test]
    fn test_rule_with_proto_any() {
        assert_eq!(
            parse_str_to_rules("pass in;", &test_filter_routines()),
            Ok(vec![filter_ext::Rule {
                id: filter_ext::RuleId { routine: local_ingress_routine(), index: 0 },
                matchers: filter_ext::Matchers::default(),
                action: filter_ext::Action::Accept,
            }])
        );
    }

    #[test]
    fn test_rule_local_ingress_without_corresponding_routine() {
        assert_eq!(
            parse_str_to_rules("pass in;", &FilterRoutines::default()),
            Err(Error::RoutineNotProvided(Direction::LocalIngress))
        );
    }

    #[test]
    fn test_rule_local_egress_without_corresponding_routine() {
        assert_eq!(
            parse_str_to_rules("pass out;", &FilterRoutines::default()),
            Err(Error::RoutineNotProvided(Direction::LocalEgress))
        );
    }

    #[test]
    fn test_rule_with_proto_tcp() {
        assert_eq!(
            parse_str_to_rules("pass in proto tcp;", &test_filter_routines()),
            Ok(vec![filter_ext::Rule {
                id: filter_ext::RuleId { routine: local_ingress_routine(), index: 0 },
                matchers: filter_ext::Matchers {
                    transport_protocol: Some(filter_ext::TransportProtocolMatcher::Tcp {
                        src_port: None,
                        dst_port: None,
                    }),
                    ..Default::default()
                },
                action: filter_ext::Action::Accept,
            }])
        )
    }

    #[test]
    fn test_multiple_rules() {
        assert_eq!(
            parse_str_to_rules("pass in proto tcp; drop out proto udp;", &test_filter_routines()),
            Ok(vec![
                filter_ext::Rule {
                    id: filter_ext::RuleId { routine: local_ingress_routine(), index: 0 },
                    matchers: filter_ext::Matchers {
                        transport_protocol: Some(filter_ext::TransportProtocolMatcher::Tcp {
                            src_port: None,
                            dst_port: None,
                        }),
                        ..Default::default()
                    },
                    action: filter_ext::Action::Accept,
                },
                filter_ext::Rule {
                    id: filter_ext::RuleId { routine: local_egress_routine(), index: 1 },
                    matchers: filter_ext::Matchers {
                        transport_protocol: Some(filter_ext::TransportProtocolMatcher::Udp {
                            src_port: None,
                            dst_port: None,
                        }),
                        ..Default::default()
                    },
                    action: filter_ext::Action::Drop,
                }
            ])
        )
    }

    #[test]
    fn test_rule_with_device_class() {
        assert_eq!(
            parse_str_to_rules("pass in proto tcp devclass ap;", &test_filter_routines()),
            Ok(vec![filter_ext::Rule {
                id: filter_ext::RuleId { routine: local_ingress_routine(), index: 0 },
                matchers: filter_ext::Matchers {
                    in_interface: Some(filter_ext::InterfaceMatcher::DeviceClass(
                        filter_ext::DeviceClass::Device(fhnet::DeviceClass::WlanAp.into())
                    )),
                    transport_protocol: Some(filter_ext::TransportProtocolMatcher::Tcp {
                        src_port: None,
                        dst_port: None,
                    }),
                    ..Default::default()
                },
                action: filter_ext::Action::Accept,
            }])
        )
    }

    // Ensure the `log` and `state` fields that are used in `filter_deprecated`
    // can be provided, but have no impact on the parsed rule. These fields
    // have no equivalent in filter2.
    #[test]
    fn test_rule_with_unused_fields() {
        assert_eq!(
            parse_str_to_rules("pass in proto tcp log no state;", &test_filter_routines()),
            Ok(vec![filter_ext::Rule {
                id: filter_ext::RuleId { routine: local_ingress_routine(), index: 0 },
                matchers: filter_ext::Matchers {
                    transport_protocol: Some(filter_ext::TransportProtocolMatcher::Tcp {
                        src_port: None,
                        dst_port: None,
                    }),
                    ..Default::default()
                },
                action: filter_ext::Action::Accept,
            }])
        )
    }
}
