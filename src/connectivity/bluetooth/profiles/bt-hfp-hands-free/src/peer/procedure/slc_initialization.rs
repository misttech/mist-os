// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Error};
use at_commands as at;

use super::Procedure;

use crate::features::{extract_features_from_command, AgFeatures, CVSD, MSBC};
use crate::peer::indicators::{BATTERY_LEVEL, ENHANCED_SAFETY, INDICATOR_REPORTING_MODE};
use crate::peer::procedure_manipulated_state::ProcedureManipulatedState;

#[derive(Debug, PartialEq)]
pub enum State {
    SupportedFeaturesExchange, // Sent AT+BRSF, waiting for +BRSF.
    WaitingForOkAfterSupportedFeaturesExchange, // Received +BRSF, waiting for OK.
    CodecNegotiation,          // Sent AT+BAC, waiting for OK.
    TestAgIndicators,          // Sent AT+CIND=?, waiting for first +CIND
    WaitingForOkAfterTestAgIndicators, // Waiting for OK after +CIND
    ReadAgIndicators,          // Sent AT+CIND?, waiting for second +CIND
    WaitingForOkAfterReadAgIndicators, // Waiting for OL after second +CIND
    AgIndicatorStatusUpdate,   // Sent AT+CMER, waiting for OK.
    CallHoldAndMultiParty,     // Sent AT+CHLD, waiting for +CHLD
    WaitingForOkAfterCallHoldAndMultiParty, // Waiting for OK after +CHLD
    HfSupportedHfIndicators,   // Sent AT+BIND, waiting for OK.
    TestHfIndicators,          // Sent AT+BIND=?, waiting for +BIND.
    WaitingForOkAfterTestHfIndicators, // Waiting for OK after +BIND.
    ReadHfIndicators,          // Sent AT+BIND?, waiting for +BIND or OK.
    Terminated,
}

#[derive(Debug)]
pub struct SlcInitProcedure {
    state: State,
}

impl SlcInitProcedure {
    pub fn new() -> Self {
        Self { state: State::SupportedFeaturesExchange }
    }

    #[cfg(test)]
    pub fn start_at_state(state: State) -> Self {
        Self { state, ..SlcInitProcedure::new() }
    }

    #[cfg(test)]
    pub fn start_terminated() -> Self {
        Self { state: State::Terminated }
    }
}

impl Procedure for SlcInitProcedure {
    fn name(&self) -> &str {
        "SLC Initialization"
    }

    /// Checks for sequential ordering of commands by first checking the
    /// stage the SLCI is in and then extract important data from AG responses
    /// and proceed to next stage if necessary.
    fn transition(
        &mut self,
        state: &mut ProcedureManipulatedState,
        update: &Vec<at::Response>,
    ) -> Result<Vec<at::Command>, Error> {
        if self.is_terminated() {
            return Err(format_err!("Procedure is already terminated at {:?} stage before processing update for response(s): {:?}.", self.state, update));
        }

        let output = match (&self.state, &update[..]) {
            // Sent AT+BRSF, waiting for +BRSF /////////////////////////////////////////////////////
            (
                State::SupportedFeaturesExchange,
                [at::Response::Success(at::Success::Brsf { features })],
            ) => {
                state.ag_features = AgFeatures::from_bits_truncate(*features);
                self.state = State::WaitingForOkAfterSupportedFeaturesExchange;
                vec![]
            }

            // Received +BSRF, waiting for OK //////////////////////////////////////////////////////
            (State::WaitingForOkAfterSupportedFeaturesExchange, [at::Response::Ok]) => {
                if state.supports_codec_negotiation() {
                    self.state = State::CodecNegotiation;
                    state.supported_codecs.push(MSBC);
                    // TODO(fxb/130963) Make this configurable.
                    // By default, we support the CVSD and MSBC codecs.
                    vec![at::Command::Bac { codecs: vec![CVSD.into(), MSBC.into()] }]
                } else {
                    self.state = State::TestAgIndicators;
                    vec![at::Command::CindTest {}]
                }
            }

            // Sent AT+BAC, waiting for OK /////////////////////////////////////////////////////////
            (State::CodecNegotiation, [at::Response::Ok]) => {
                self.state = State::TestAgIndicators;
                vec![at::Command::CindTest {}]
            }

            // Sent AT+CIND=?, waiting for +CIND: //////////////////////////////////////////////////
            (State::TestAgIndicators, [at::Response::RawBytes(_bytes)]) => {
                // TODO(fxbug.dev/108331): Read additional indicators by parsing raw bytes instead
                // ofjust checking for existence of raw bytes.
                self.state = State::WaitingForOkAfterTestAgIndicators;
                vec![]
            }

            // Received +CIND, waiting for OK /////////////////////////////////////////////////////
            (State::WaitingForOkAfterTestAgIndicators, [at::Response::Ok]) => {
                self.state = State::ReadAgIndicators;
                vec![at::Command::CindRead {}]
            }

            // Sent AT+CIND?, waiting for +CIND: ///////////////////////////////////////////////////
            (State::ReadAgIndicators, [at::Response::Success(cmd @ at::Success::Cind { .. })]) => {
                state.ag_indicators.update_indicator_values(&cmd);
                self.state = State::WaitingForOkAfterReadAgIndicators;
                vec![]
            }

            // Received +CIND:, waiting for OK /////////////////////////////////////////////////////
            (State::WaitingForOkAfterReadAgIndicators, [at::Response::Ok]) => {
                self.state = State::AgIndicatorStatusUpdate;
                vec![at::Command::Cmer { mode: INDICATOR_REPORTING_MODE, keyp: 0, disp: 0, ind: 1 }]
            }

            // Sent AT+CMER, waiting for OK ////////////////////////////////////////////////////////
            (State::AgIndicatorStatusUpdate, [at::Response::Ok]) => {
                if state.supports_three_way_calling() {
                    self.state = State::CallHoldAndMultiParty;
                    vec![at::Command::ChldTest {}]
                } else if state.supports_hf_indicators() {
                    self.state = State::HfSupportedHfIndicators;
                    vec![at::Command::Bind {
                        indicators: vec![ENHANCED_SAFETY as i64, BATTERY_LEVEL as i64],
                    }]
                } else {
                    self.state = State::Terminated;
                    vec![]
                }
            }

            // Sent AT+CHLD=?, waiting for +CHLD: //////////////////////////////////////////////////
            (
                State::CallHoldAndMultiParty,
                [at::Response::Success(at::Success::Chld { commands })],
            ) => {
                state.three_way_features = extract_features_from_command(&commands)?;
                self.state = State::WaitingForOkAfterCallHoldAndMultiParty;
                vec![]
            }

            // Received +CHLD:, waiting for OK /////////////////////////////////////////////////////
            (State::WaitingForOkAfterCallHoldAndMultiParty, [at::Response::Ok]) => {
                if state.supports_hf_indicators() {
                    self.state = State::HfSupportedHfIndicators;
                    vec![at::Command::Bind {
                        indicators: vec![ENHANCED_SAFETY as i64, BATTERY_LEVEL as i64],
                    }]
                } else {
                    self.state = State::Terminated;
                    vec![]
                }
            }

            // Sent AT+BIND=, waiting for OK ///////////////////////////////////////////////////////
            (State::HfSupportedHfIndicators, [at::Response::Ok]) => {
                self.state = State::TestHfIndicators;
                vec![at::Command::BindTest {}]
            }

            // Sent AT+BIND=?, waiting for +BIND: ///////////////////////////////////////////////////
            (
                State::TestHfIndicators,
                [at::Response::Success(at::Success::BindList { indicators })],
            ) => {
                state.hf_indicators.set_supported_indicators(indicators);
                self.state = State::WaitingForOkAfterTestHfIndicators;
                vec![]
            }

            // Received +BIND:, waiting for OK /////////////////////////////////////////////////////
            (State::WaitingForOkAfterTestHfIndicators, [at::Response::Ok]) => {
                self.state = State::ReadHfIndicators;
                vec![at::Command::BindRead {}]
            }

            // Sent AT+BIND?, waiting for +BIND: or OK /////////////////////////////////////////////
            (
                State::ReadHfIndicators,
                [at::Response::Success(cmd @ at::Success::BindStatus { .. })],
            ) => {
                state.hf_indicators.change_indicator_state(&cmd)?;
                vec![]
            }

            // Sent AT+BIND?, waiting for +BIND: or OK /////////////////////////////////////////////
            (State::ReadHfIndicators, [at::Response::Ok]) => {
                self.state = State::Terminated;
                vec![]
            }

            // Unexpected AT response for state ////////////////////////////////////////////////////
            _ => {
                // Early return for error
                return Err(format_err!(
                    "Wrong responses at {:?} stage of SLCI with response(s): {:?}.",
                    self.state,
                    update
                ));
            }
        };

        Ok(output)
    }

    fn is_terminated(&self) -> bool {
        self.state == State::Terminated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{config::HandsFreeFeatureSupport, features::CallHoldAction, features::HfFeatures};
    use assert_matches::assert_matches;

    // TODO(fxb/71668) Stop using raw bytes.
    const CIND_TEST_RESPONSE_BYTES: &[u8] = b"+CIND: \
    (\"service\",(0,1)),\
    (\"call\",(0,1)),\
    (\"callsetup\",(0,3)),\
    (\"callheld\",(0,2)),\
    (\"signal\",(0,5)),\
    (\"roam\",(0,1)),\
    (\"battchg\",(0,5)\
    )";

    #[fuchsia::test]
    /// Checks that the mandatory exchanges between the AG and HF roles properly progresses
    /// our state and sends the expected responses until our procedure it marked complete.
    fn slci_mandatory_exchanges_and_termination() {
        let mut procedure = SlcInitProcedure::new();
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        let response1 = vec![at::Response::Success(at::Success::Brsf {
            features: AgFeatures::default().bits(),
        })];
        let response1_ok = vec![at::Response::Ok];
        let expected_command1 = vec![at::Command::CindTest {}];

        assert_eq!(procedure.transition(&mut state, &response1).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, &response1_ok).unwrap(), expected_command1);

        let indicator_msg = CIND_TEST_RESPONSE_BYTES.to_vec();
        let response2 = vec![at::Response::RawBytes(indicator_msg)];
        let response2_ok = vec![at::Response::Ok];
        let expected_command2 = vec![at::Command::CindRead {}];
        assert_eq!(procedure.transition(&mut state, &response2).unwrap(), vec![]);

        assert_eq!(procedure.transition(&mut state, &response2_ok).unwrap(), expected_command2);

        let response3 = vec![at::Response::Success(at::Success::Cind {
            service: false,
            call: false,
            callsetup: 0,
            callheld: 0,
            signal: 0,
            roam: false,
            battchg: 0,
        })];
        let response3_ok = vec![at::Response::Ok];
        let update3 =
            vec![at::Command::Cmer { mode: INDICATOR_REPORTING_MODE, keyp: 0, disp: 0, ind: 1 }];
        assert_eq!(procedure.transition(&mut state, &response3).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, &response3_ok).unwrap(), update3);

        let response4 = vec![at::Response::Ok];
        assert_eq!(procedure.transition(&mut state, &response4).unwrap(), vec![]);

        assert!(procedure.is_terminated());
    }

    #[fuchsia::test]
    fn slci_hf_indicator_properly_works() {
        let mut procedure = SlcInitProcedure::new();
        // Hf indicators needed for optional procedure.
        let mut hf_features = HfFeatures::default();
        let mut ag_features = AgFeatures::default();
        hf_features.set(HfFeatures::HF_INDICATORS, true);
        ag_features.set(AgFeatures::HF_INDICATORS, true);
        let mut state = ProcedureManipulatedState::load_with_set_features(hf_features, ag_features);

        assert!(!state.hf_indicators.enhanced_safety.1);
        assert!(!state.hf_indicators.battery_level.1);
        assert!(!state.hf_indicators.enhanced_safety.0.enabled);
        assert!(!state.hf_indicators.battery_level.0.enabled);
        assert!(!procedure.is_terminated());

        let response1 =
            vec![at::Response::Success(at::Success::Brsf { features: ag_features.bits() })];
        let response1_ok = vec![at::Response::Ok];
        let expected_command1 = vec![at::Command::CindTest {}];

        assert_eq!(procedure.transition(&mut state, &response1).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, &response1_ok).unwrap(), expected_command1);

        let indicator_msg = CIND_TEST_RESPONSE_BYTES.to_vec();
        let response2 = vec![at::Response::RawBytes(indicator_msg)];
        let response2_ok = vec![at::Response::Ok];
        let expected_command2 = vec![at::Command::CindRead {}];

        assert_eq!(procedure.transition(&mut state, &response2).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, &response2_ok).unwrap(), expected_command2);

        let response3 = vec![at::Response::Success(at::Success::Cind {
            service: false,
            call: false,
            callsetup: 0,
            callheld: 0,
            signal: 0,
            roam: false,
            battchg: 0,
        })];
        let response3_ok = vec![at::Response::Ok];
        let expected_command3 =
            vec![at::Command::Cmer { mode: INDICATOR_REPORTING_MODE, keyp: 0, disp: 0, ind: 1 }];

        assert_eq!(procedure.transition(&mut state, &response3).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, &response3_ok).unwrap(), expected_command3);

        let response4 = vec![at::Response::Ok];
        let expected_command4 = vec![at::Command::Bind {
            indicators: vec![ENHANCED_SAFETY as i64, BATTERY_LEVEL as i64],
        }];
        assert_eq!(procedure.transition(&mut state, &response4).unwrap(), expected_command4);

        let response5 = vec![at::Response::Ok];
        let expected_command5 = vec![at::Command::BindTest {}];
        assert_eq!(procedure.transition(&mut state, &response5).unwrap(), expected_command5);

        let response6 = vec![at::Response::Success(at::Success::BindList {
            indicators: vec![
                at::BluetoothHFIndicator::BatteryLevel,
                at::BluetoothHFIndicator::EnhancedSafety,
            ],
        })];
        let response6_ok = vec![at::Response::Ok];
        let expected_command6 = vec![at::Command::BindRead {}];
        assert_eq!(procedure.transition(&mut state, &response6).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, &response6_ok).unwrap(), expected_command6);
        assert!(state.hf_indicators.enhanced_safety.1);
        assert!(state.hf_indicators.battery_level.1);

        let response7 = vec![at::Response::Success(at::Success::BindStatus {
            anum: at::BluetoothHFIndicator::EnhancedSafety,
            state: true,
        })];
        let response8 = vec![at::Response::Success(at::Success::BindStatus {
            anum: at::BluetoothHFIndicator::BatteryLevel,
            state: true,
        })];
        let response9 = vec![at::Response::Ok];
        assert_eq!(procedure.transition(&mut state, &response7).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, &response8).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, &response9).unwrap(), vec![]);
        assert!(state.hf_indicators.enhanced_safety.0.enabled);
        assert!(state.hf_indicators.battery_level.0.enabled);
    }

    #[fuchsia::test]
    fn slci_codec_negotiation_properly_works() {
        let mut procedure = SlcInitProcedure::new();
        let mut hf_features = HfFeatures::default();
        hf_features.set(HfFeatures::CODEC_NEGOTIATION, true);
        let mut ag_features = AgFeatures::default();
        ag_features.set(AgFeatures::CODEC_NEGOTIATION, true);
        let mut state = ProcedureManipulatedState::load_with_set_features(hf_features, ag_features);

        assert!(!procedure.is_terminated());

        let response1 =
            vec![at::Response::Success(at::Success::Brsf { features: ag_features.bits() })];
        let response1_ok = vec![at::Response::Ok];
        let expected_command1 = vec![at::Command::Bac { codecs: vec![CVSD.into(), MSBC.into()] }];

        assert_eq!(procedure.transition(&mut state, &response1).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, &response1_ok).unwrap(), expected_command1);

        let response2 = vec![at::Response::Ok];
        let expected_command2 = vec![at::Command::CindTest {}];

        assert_eq!(procedure.transition(&mut state, &response2).unwrap(), expected_command2);
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn slci_three_way_feature_proper_works() {
        let mut procedure = SlcInitProcedure::new();
        // Three way calling needed for stage progression.
        let mut hf_features = HfFeatures::default();
        hf_features.set(HfFeatures::THREE_WAY_CALLING, true);
        let mut ag_features = AgFeatures::default();
        ag_features.set(AgFeatures::THREE_WAY_CALLING, true);
        let mut state = ProcedureManipulatedState::load_with_set_features(hf_features, ag_features);

        assert!(!procedure.is_terminated());

        let response1 =
            vec![at::Response::Success(at::Success::Brsf { features: ag_features.bits() })];
        let response1_ok = vec![at::Response::Ok];
        let expected_command1 = vec![at::Command::CindTest {}];

        assert_eq!(procedure.transition(&mut state, &response1).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, &response1_ok).unwrap(), expected_command1);

        let indicator_msg = CIND_TEST_RESPONSE_BYTES.to_vec();
        let response2 = vec![at::Response::RawBytes(indicator_msg)];
        let response2_ok = vec![at::Response::Ok];
        let expected_command2 = vec![at::Command::CindRead {}];
        assert_eq!(procedure.transition(&mut state, &response2).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, &response2_ok).unwrap(), expected_command2);

        let response3 = vec![at::Response::Success(at::Success::Cind {
            service: false,
            call: false,
            callsetup: 0,
            callheld: 0,
            signal: 0,
            roam: false,
            battchg: 0,
        })];
        let response3_ok = vec![at::Response::Ok];
        let update3 =
            vec![at::Command::Cmer { mode: INDICATOR_REPORTING_MODE, keyp: 0, disp: 0, ind: 1 }];
        assert_eq!(procedure.transition(&mut state, &response3).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, &response3_ok).unwrap(), update3);

        let response4 = vec![at::Response::Ok];
        let update4 = vec![at::Command::ChldTest {}];
        assert_eq!(procedure.transition(&mut state, &response4).unwrap(), update4);

        let commands = vec![
            String::from("0"),
            String::from("1"),
            String::from("2"),
            String::from("11"),
            String::from("22"),
            String::from("3"),
            String::from("4"),
        ];
        let response5 = vec![at::Response::Success(at::Success::Chld { commands })];
        let response5_ok = vec![at::Response::Ok];
        assert_eq!(procedure.transition(&mut state, &response5).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, &response5_ok).unwrap(), vec![]);
        assert!(procedure.is_terminated());

        let features = vec![
            CallHoldAction::ReleaseAllHeld,
            CallHoldAction::ReleaseAllActive,
            CallHoldAction::HoldActiveAndAccept,
            CallHoldAction::ReleaseSpecified(1),
            CallHoldAction::HoldAllExceptSpecified(2),
            CallHoldAction::AddCallToHeldConversation,
            CallHoldAction::ExplicitCallTransfer,
        ];

        assert_eq!(features, state.three_way_features);
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_feature_stage() {
        let mut procedure = SlcInitProcedure::start_at_state(State::SupportedFeaturesExchange);
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        let wrong_response = vec![at::Response::Success(at::Success::TestResponse {})];
        assert_matches!(procedure.transition(&mut state, &wrong_response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_codec_negotiation_stage() {
        let mut procedure = SlcInitProcedure::start_at_state(State::CodecNegotiation);
        let mut hf_features = HfFeatures::default();
        hf_features.set(HfFeatures::CODEC_NEGOTIATION, true);
        let mut ag_features = AgFeatures::default();
        ag_features.set(AgFeatures::CODEC_NEGOTIATION, true);
        let mut state = ProcedureManipulatedState::load_with_set_features(hf_features, ag_features);

        assert!(!procedure.is_terminated());

        let wrong_response = vec![at::Response::Success(at::Success::TestResponse {})];
        assert_matches!(procedure.transition(&mut state, &wrong_response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_list_indicators_stage() {
        let mut procedure = SlcInitProcedure::start_at_state(State::TestAgIndicators);
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        let wrong_response = vec![at::Response::Success(at::Success::TestResponse {})];
        assert_matches!(procedure.transition(&mut state, &wrong_response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_enable_indicators_stage() {
        let mut procedure =
            SlcInitProcedure::start_at_state(State::WaitingForOkAfterReadAgIndicators);
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        let wrong_response = vec![at::Response::Success(at::Success::TestResponse {})];
        assert_matches!(procedure.transition(&mut state, &wrong_response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_indicator_update_stage() {
        let mut procedure = SlcInitProcedure::start_at_state(State::AgIndicatorStatusUpdate);
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        let wrong_response = vec![at::Response::Success(at::Success::TestResponse {})];
        assert_matches!(procedure.transition(&mut state, &wrong_response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_call_hold_stage_non_number_index() {
        let mut procedure = SlcInitProcedure::start_at_state(State::CallHoldAndMultiParty);
        let config = HandsFreeFeatureSupport {
            call_waiting_or_three_way_calling: true,
            ..HandsFreeFeatureSupport::default()
        };
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        let invalid_command = vec![String::from("1A")];
        let response = vec![at::Response::Success(at::Success::Chld { commands: invalid_command })];

        assert_matches!(procedure.transition(&mut state, &response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_call_hold_stage_invalid_command() {
        let mut procedure = SlcInitProcedure::start_at_state(State::CallHoldAndMultiParty);
        let config = HandsFreeFeatureSupport {
            call_waiting_or_three_way_calling: true,
            ..HandsFreeFeatureSupport::default()
        };
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        let invalid_command = vec![String::from("5")];
        let response = vec![at::Response::Success(at::Success::Chld { commands: invalid_command })];

        assert_matches!(procedure.transition(&mut state, &response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_hf_indicator_stage() {
        let mut procedure = SlcInitProcedure::start_at_state(State::HfSupportedHfIndicators);
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        // Did not receive expected Ok response as should result in error.
        let wrong_response = vec![at::Response::Success(at::Success::TestResponse {})];
        assert_matches!(procedure.transition(&mut state, &wrong_response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_hf_indicator_request_stage() {
        let mut procedure = SlcInitProcedure::start_at_state(State::TestHfIndicators);
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        // Did not receive expected Ok response as should result in error.
        let wrong_response = vec![at::Response::Success(at::Success::TestResponse {})];
        assert_matches!(procedure.transition(&mut state, &wrong_response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_hf_indicator_enable_stage() {
        let mut procedure = SlcInitProcedure::start_at_state(State::ReadHfIndicators);
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        // Did not receive expected Ok response as should result in error.
        let wrong_response = vec![at::Response::Success(at::Success::TestResponse {})];
        assert_matches!(procedure.transition(&mut state, &wrong_response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_no_ok_at_hf_indicator_enable_stage() {
        let mut procedure = SlcInitProcedure::start_at_state(State::ReadHfIndicators);
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        let wrong_response = vec![at::Response::Success(at::Success::TestResponse {})];
        assert_matches!(procedure.transition(&mut state, &wrong_response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_update_on_terminated_procedure() {
        let mut procedure = SlcInitProcedure::start_terminated();
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(procedure.is_terminated());
        // Valid response of first step of SLCI
        let valid_response = vec![at::Response::Success(at::Success::Brsf {
            features: AgFeatures::default().bits(),
        })];
        let update = procedure.transition(&mut state, &valid_response);
        assert_matches!(update, Err(_));
    }
}
