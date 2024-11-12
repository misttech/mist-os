// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use fidl_fuchsia_wlan_fullmac::{self as fidl_fullmac, WlanFullmacImpl_Request};
use futures::StreamExt;

// Wrapper type for WlanFullmacImpl_Request types without the responder.
#[derive(Clone, Debug, PartialEq)]
pub enum FullmacRequest {
    Query,
    QueryMacSublayerSupport,
    QuerySecuritySupport,
    StartScan(fidl_fullmac::WlanFullmacImplStartScanRequest),
    Connect(fidl_fullmac::WlanFullmacImplConnectRequest),
    Reconnect(fidl_fullmac::WlanFullmacImplReconnectRequest),
    AuthResp(fidl_fullmac::WlanFullmacImplAuthRespRequest),
    Deauth(fidl_fullmac::WlanFullmacImplDeauthRequest),
    AssocResp(fidl_fullmac::WlanFullmacImplAssocRespRequest),
    Disassoc(fidl_fullmac::WlanFullmacImplDisassocRequest),
    Reset(fidl_fullmac::WlanFullmacImplResetRequest),
    StartBss(fidl_fullmac::WlanFullmacImplStartBssRequest),
    StopBss(fidl_fullmac::WlanFullmacImplStopBssRequest),
    SetKeys(fidl_fullmac::WlanFullmacImplSetKeysRequest),
    DelKeys(fidl_fullmac::WlanFullmacImplDelKeysRequest),
    EapolTx(fidl_fullmac::WlanFullmacImplEapolTxRequest),
    GetIfaceCounterStats,
    GetIfaceHistogramStats,
    SaeHandshakeResp(fidl_fullmac::WlanFullmacSaeHandshakeResp),
    SaeFrameTx(fidl_fullmac::WlanFullmacSaeFrame),
    WmmStatusReq,
    SetMulticastPromisc(bool),
    OnLinkStateChanged(bool),

    // Note: WlanFullmacImpl::Start has a channel as an argument, but we don't keep the channel
    // here.
    Start,
}

/// A wrapper around WlanFullmacImpl_RequestStream that records each handled request in its
/// |history|. Users of this type should not access |request_stream| directly; instead, use
/// RecordedRequestStream::handle_request.
pub struct RecordedRequestStream {
    request_stream: fidl_fullmac::WlanFullmacImpl_RequestStream,
    history: Vec<FullmacRequest>,
}

impl RecordedRequestStream {
    pub fn new(request_stream: fidl_fullmac::WlanFullmacImpl_RequestStream) -> Self {
        Self { request_stream, history: Vec::new() }
    }

    pub fn history(&self) -> &[FullmacRequest] {
        &self.history[..]
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    /// Retrieves a single request from the request stream.
    /// This records the request type in its history (copying the request payload out if one
    /// exists) before returning it.
    pub async fn next(&mut self) -> fidl_fullmac::WlanFullmacImpl_Request {
        let request = self
            .request_stream
            .next()
            .await
            .unwrap()
            .expect("Could not get next request in fullmac request stream");
        match &request {
            WlanFullmacImpl_Request::Query { .. } => self.history.push(FullmacRequest::Query),
            WlanFullmacImpl_Request::QueryMacSublayerSupport { .. } => {
                self.history.push(FullmacRequest::QueryMacSublayerSupport)
            }
            WlanFullmacImpl_Request::QuerySecuritySupport { .. } => {
                self.history.push(FullmacRequest::QuerySecuritySupport)
            }
            WlanFullmacImpl_Request::StartScan { payload, .. } => {
                self.history.push(FullmacRequest::StartScan(payload.clone()))
            }
            WlanFullmacImpl_Request::Connect { payload, .. } => {
                self.history.push(FullmacRequest::Connect(payload.clone()))
            }
            WlanFullmacImpl_Request::Reconnect { payload, .. } => {
                self.history.push(FullmacRequest::Reconnect(payload.clone()))
            }
            WlanFullmacImpl_Request::AuthResp { payload, .. } => {
                self.history.push(FullmacRequest::AuthResp(payload.clone()))
            }
            WlanFullmacImpl_Request::Deauth { payload, .. } => {
                self.history.push(FullmacRequest::Deauth(payload.clone()))
            }
            WlanFullmacImpl_Request::AssocResp { payload, .. } => {
                self.history.push(FullmacRequest::AssocResp(payload.clone()))
            }
            WlanFullmacImpl_Request::Disassoc { payload, .. } => {
                self.history.push(FullmacRequest::Disassoc(payload.clone()))
            }
            WlanFullmacImpl_Request::Reset { payload, .. } => {
                self.history.push(FullmacRequest::Reset(payload.clone()))
            }
            WlanFullmacImpl_Request::StartBss { payload, .. } => {
                self.history.push(FullmacRequest::StartBss(payload.clone()))
            }
            WlanFullmacImpl_Request::StopBss { payload, .. } => {
                self.history.push(FullmacRequest::StopBss(payload.clone()))
            }
            WlanFullmacImpl_Request::SetKeys { payload, .. } => {
                self.history.push(FullmacRequest::SetKeys(payload.clone()))
            }
            WlanFullmacImpl_Request::DelKeys { payload, .. } => {
                self.history.push(FullmacRequest::DelKeys(payload.clone()))
            }
            WlanFullmacImpl_Request::EapolTx { payload, .. } => {
                self.history.push(FullmacRequest::EapolTx(payload.clone()))
            }
            WlanFullmacImpl_Request::GetIfaceCounterStats { .. } => {
                self.history.push(FullmacRequest::GetIfaceCounterStats)
            }
            WlanFullmacImpl_Request::GetIfaceHistogramStats { .. } => {
                self.history.push(FullmacRequest::GetIfaceHistogramStats)
            }
            WlanFullmacImpl_Request::SaeHandshakeResp { resp, .. } => {
                self.history.push(FullmacRequest::SaeHandshakeResp(resp.clone()))
            }
            WlanFullmacImpl_Request::SaeFrameTx { frame, .. } => {
                self.history.push(FullmacRequest::SaeFrameTx(frame.clone()))
            }
            WlanFullmacImpl_Request::WmmStatusReq { .. } => {
                self.history.push(FullmacRequest::WmmStatusReq)
            }
            WlanFullmacImpl_Request::SetMulticastPromisc { enable, .. } => {
                self.history.push(FullmacRequest::SetMulticastPromisc(*enable))
            }
            WlanFullmacImpl_Request::OnLinkStateChanged { online, .. } => {
                self.history.push(FullmacRequest::OnLinkStateChanged(*online))
            }
            WlanFullmacImpl_Request::Start { .. } => self.history.push(FullmacRequest::Start),

            _ => panic!("Unrecognized Fullmac request {:?}", request),
        }
        request
    }
}
