// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Result};
use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::character::complete::{alpha1, char, digit1};
use nom::combinator::map_res;
use nom::error::ErrorKind;
use nom::multi::separated_list0;
use nom::sequence::{delimited, preceded, separated_pair};
use nom::Parser;

use crate::peer::ag_indicators::{check_ag_indicator_range_allowed, AgIndicatorIndex};
use crate::peer::at_connection::Response as AtResponse;

// TODO(b/417756085) Evaluate rewriting this without nom.

/// Parses an AT response of the form
/// +CIND: (<ag_indicator_name_0>,(<min_value_0>,<max_value_0)),...,(<ag_indicator_name_k>,(<min_value_k>,<max_value_k))
/// as specified in ETSI TS 127 007 v6.8.0 sec. 8.9 and HFP v1.8 sec. 4.32.2: AT+CIND into an
/// ordered vector of AG indicators. In doing so it checks that the ranges provided in the AT
/// response by the peer match those required by the HFP spec.
pub fn parse(bytes: Vec<u8>) -> Result<AtResponse> {
    let mut string = String::from_utf8(bytes)?; // AT commands are ASCII.
    string.retain(|c| !char::is_whitespace(c)); // Strip whitespace.
    let str = string.as_str();

    // This line parses the response name, "+CIND:".
    let name_parser = tag::<&str, &str, (&str, ErrorKind)>("+CIND:");

    // The next two lines parse a double quote delimited name of an indicator into an
    // AgIndicatorIndex.
    let ag_indicator_name_parser = delimited(char('"'), alpha1, char('"'));
    let ag_indicator_parser = map_res(ag_indicator_name_parser, AgIndicatorIndex::try_from);

    // The next five lines parse a parenthesis delimited pair of integers such as "(0,1)"
    // or "(0-1)" indicating a range of indicator values.
    let min_parser = map_res(digit1, |n: &str| n.parse::<i64>());
    let max_parser = map_res(digit1, |n: &str| n.parse::<i64>());

    let comma_or_dash_parser = alt((char(','), char('-')));
    let range_parser = separated_pair(min_parser, comma_or_dash_parser, max_parser);
    let delimited_range_parser = delimited(char('('), range_parser, char(')'));

    // The next two lines parse a parenthesis delimited pair of indicator names and ranges.
    let pair_parser = separated_pair(ag_indicator_parser, char(','), delimited_range_parser);
    let delimited_pair_parser = delimited(char('('), pair_parser, char(')'));

    // This line parses a comma separated list of such pairs
    let pairs_parser = separated_list0(char(','), delimited_pair_parser);

    // This line parses the name followed by such a list, i.e, the whole response.
    let mut parser = preceded(name_parser, pairs_parser);

    let (rest, indicators_and_ranges) =
        parser.parse(str).map_err(|err| format_err!("+CIND parse error: {:?}", err))?;

    if !rest.is_empty() {
        Err(format_err!(
            "Had characters {:?} left over after parsing possible AT+CIND message {:}",
            rest,
            string
        ))?
    }

    // Check that the indicator ranges provided by the peer are those specified in the HFP spec.
    // If not, the peer is nonconformant.
    for (indicator, (min, max)) in indicators_and_ranges.iter() {
        check_ag_indicator_range_allowed(*indicator, *min, *max)?;
    }

    let indicators = indicators_and_ranges.iter().map(|ir| ir.0).collect();
    Ok(AtResponse::CindTest { ordered_indicators: indicators })
}

#[cfg(test)]
mod tests {
    use super::*;

    use assert_matches::assert_matches;

    #[fuchsia::test]
    fn parse_successfully() {
        let bytes = "+CIND: (\"service\",(0,1)),(\"call\",(0,1)),(\"callsetup\",(0,3)),(\"callheld\",(0,2)),(\"signal\",(0,5)),(\"roam\",(0,1)),(\"battchg\",(0,5))";
        let parsed = parse(Vec::from(bytes)).expect("Parsing");
        assert_eq!(
            parsed,
            AtResponse::CindTest {
                ordered_indicators: vec![
                    AgIndicatorIndex::ServiceAvailable,
                    AgIndicatorIndex::Call,
                    AgIndicatorIndex::CallSetup,
                    AgIndicatorIndex::CallHeld,
                    AgIndicatorIndex::SignalStrength,
                    AgIndicatorIndex::Roaming,
                    AgIndicatorIndex::BatteryCharge,
                ]
            }
        )
    }

    #[fuchsia::test]
    fn small_parse_successfully() {
        let bytes = b"+CIND: (\"service\",(0,1))";
        let parsed = parse(Vec::from(bytes)).expect("Parsing");
        assert_eq!(
            parsed,
            AtResponse::CindTest { ordered_indicators: vec![AgIndicatorIndex::ServiceAvailable,] }
        )
    }

    #[fuchsia::test]
    fn whitespace_allowed() {
        let bytes = b"+CIND: (\" service \" , ( 0 , 1 ) )";
        let parsed = parse(Vec::from(bytes)).expect("Parsing");
        assert_eq!(
            parsed,
            AtResponse::CindTest { ordered_indicators: vec![AgIndicatorIndex::ServiceAvailable] }
        )
    }

    #[fuchsia::test]
    fn extra_characters() {
        let bytes = b"+CIND: (\"service\",(0,1))XXX";
        let parsed = parse(Vec::from(bytes));
        assert_matches!(parsed, Err(_));
    }

    #[fuchsia::test]
    fn no_tag() {
        let bytes = b"(\"service\",(0,1))";
        let parsed = parse(Vec::from(bytes));
        assert_matches!(parsed, Err(_));
    }

    #[fuchsia::test]
    fn unclosed_parens() {
        let bytes = b"+CIND: (\"service\",(0,1)";
        let parsed = parse(Vec::from(bytes));
        assert_matches!(parsed, Err(_));
    }

    #[fuchsia::test]
    fn range_wrong() {
        let bytes = b"+CIND: (\"service\",(0,2))";
        let parsed = parse(Vec::from(bytes));
        assert_matches!(parsed, Err(_));
    }
}
