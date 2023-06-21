// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//go:build !build_with_native_toolchain

package main

import (
	"context"
	"errors"
	"fmt"
	"log"
	"syscall/zx"
	"syscall/zx/fidl"

	"go.fuchsia.dev/fuchsia/src/lib/component"
	syslog "go.fuchsia.dev/fuchsia/src/lib/syslog/go"

	"fidl/fidl/serversuite"
	fidlzx "fidl/zx"
)

type closedTargetControllerImpl struct {
	sutChannel zx.Channel
}

var _ serversuite.ClosedTargetControllerWithCtx = (*closedTargetControllerImpl)(nil)

func (c *closedTargetControllerImpl) CloseWithEpitaph(_ fidl.Context, epitaph int32) error {
	return component.CloseWithEpitaph(c.sutChannel, zx.Status(epitaph))
}

type closedTargetImpl struct {
	controllerEventProxy serversuite.ClosedTargetControllerEventProxy
}

var _ serversuite.ClosedTargetWithCtx = (*closedTargetImpl)(nil)

func (t *closedTargetImpl) OneWayNoPayload(_ fidl.Context) error {
	log.Println("serversuite.ClosedTarget OneWayNoPayload() called")
	t.controllerEventProxy.ReceivedOneWayNoPayload()
	return nil
}

func (t *closedTargetImpl) TwoWayNoPayload(_ fidl.Context) error {
	log.Println("serversuite.ClosedTarget TwoWayNoPayload() called")
	return nil
}

func (t *closedTargetImpl) TwoWayStructPayload(_ fidl.Context, v int8) (int8, error) {
	log.Println("serversuite.ClosedTarget TwoWayStructPayload() called")
	return v, nil
}

func (t *closedTargetImpl) TwoWayTablePayload(_ fidl.Context, request serversuite.ClosedTargetTwoWayTablePayloadRequest) (serversuite.ClosedTargetTwoWayTablePayloadResponse, error) {
	log.Println("serversuite.ClosedTarget TwoWayTablePayload() called")
	var response serversuite.ClosedTargetTwoWayTablePayloadResponse
	response.SetV(request.V)
	return response, nil
}

func (t *closedTargetImpl) TwoWayUnionPayload(_ fidl.Context, request serversuite.ClosedTargetTwoWayUnionPayloadRequest) (serversuite.ClosedTargetTwoWayUnionPayloadResponse, error) {
	log.Println("serversuite.ClosedTarget TwoWayUnionPayload() called")
	var response serversuite.ClosedTargetTwoWayUnionPayloadResponse
	response.SetV(request.V)
	return response, nil
}

func (t *closedTargetImpl) TwoWayResult(_ fidl.Context, request serversuite.ClosedTargetTwoWayResultRequest) (serversuite.ClosedTargetTwoWayResultResult, error) {
	log.Println("serversuite.ClosedTarget TwoWayResult() called")
	var result serversuite.ClosedTargetTwoWayResultResult
	switch request.Which() {
	case serversuite.ClosedTargetTwoWayResultRequestPayload:
		result.SetResponse(serversuite.ClosedTargetTwoWayResultResponse{
			Payload: request.Payload,
		})
	case serversuite.ClosedTargetTwoWayResultRequestError:
		result.SetErr(request.Error)
	}
	return result, nil
}

func (t *closedTargetImpl) GetHandleRights(_ fidl.Context, handle zx.Handle) (fidlzx.Rights, error) {
	info, err := handle.GetInfoHandleBasic()
	if err != nil {
		return 0, err
	}
	return fidlzx.Rights(info.Rights), nil
}

func (t *closedTargetImpl) GetSignalableEventRights(_ fidl.Context, handle zx.Event) (fidlzx.Rights, error) {
	info, err := handle.Handle().GetInfoHandleBasic()
	if err != nil {
		return 0, err
	}
	return fidlzx.Rights(info.Rights), nil
}

func (t *closedTargetImpl) EchoAsTransferableSignalableEvent(_ fidl.Context, handle zx.Handle) (zx.Event, error) {
	return zx.Event(handle), nil
}

func (t *closedTargetImpl) CloseWithEpitaph(_ fidl.Context, status int32) error {
	return &component.Epitaph{Status: zx.Status(status)}
}

func (t *closedTargetImpl) ByteVectorSize(_ fidl.Context, v []uint8) (uint32, error) {
	return uint32(len(v)), nil
}

func (t *closedTargetImpl) HandleVectorSize(_ fidl.Context, v []zx.Event) (uint32, error) {
	return uint32(len(v)), nil
}

func (t *closedTargetImpl) CreateNByteVector(_ fidl.Context, n uint32) ([]uint8, error) {
	return make([]uint8, n), nil
}

func (t *closedTargetImpl) CreateNHandleVector(_ fidl.Context, n uint32) ([]zx.Event, error) {
	out := make([]zx.Event, n)
	for i := range out {
		var err error
		out[i], err = zx.NewEvent(0)
		if err != nil {
			return nil, err
		}
	}
	return out, nil
}

type runnerImpl struct{}

var _ serversuite.RunnerWithCtx = (*runnerImpl)(nil)

func (*runnerImpl) IsTestEnabled(_ fidl.Context, test serversuite.Test) (bool, error) {
	isEnabled := func(test serversuite.Test) bool {
		switch test {
		// This case will forever be false, as it is intended to validate the "test
		// disabling" functionality of the runner itself.
		case serversuite.TestIgnoreDisabled:
			return false

		case serversuite.TestOneWayWithNonZeroTxid,
			serversuite.TestTwoWayNoPayloadWithZeroTxid,
			serversuite.TestSendStrictEvent,
			serversuite.TestSendFlexibleEvent,
			serversuite.TestReceiveStrictOneWay,
			serversuite.TestReceiveStrictOneWayMismatchedStrictness,
			serversuite.TestReceiveFlexibleOneWay,
			serversuite.TestReceiveFlexibleOneWayMismatchedStrictness,
			serversuite.TestStrictTwoWayResponse,
			serversuite.TestStrictTwoWayResponseMismatchedStrictness,
			serversuite.TestStrictTwoWayNonEmptyResponse,
			serversuite.TestStrictTwoWayErrorSyntaxResponse,
			serversuite.TestStrictTwoWayErrorSyntaxResponseMismatchedStrictness,
			serversuite.TestStrictTwoWayErrorSyntaxNonEmptyResponse,
			serversuite.TestFlexibleTwoWayResponse,
			serversuite.TestFlexibleTwoWayResponseMismatchedStrictness,
			serversuite.TestFlexibleTwoWayNonEmptyResponse,
			serversuite.TestFlexibleTwoWayErrorSyntaxResponseSuccessResult,
			serversuite.TestFlexibleTwoWayErrorSyntaxResponseErrorResult,
			serversuite.TestFlexibleTwoWayErrorSyntaxNonEmptyResponseSuccessResult,
			serversuite.TestFlexibleTwoWayErrorSyntaxNonEmptyResponseErrorResult,
			serversuite.TestUnknownStrictOneWayOpenProtocol,
			serversuite.TestUnknownFlexibleOneWayOpenProtocol,
			serversuite.TestUnknownFlexibleOneWayHandleOpenProtocol,
			serversuite.TestUnknownStrictTwoWayOpenProtocol,
			serversuite.TestUnknownFlexibleTwoWayOpenProtocol,
			serversuite.TestUnknownFlexibleTwoWayHandleOpenProtocol,
			serversuite.TestUnknownStrictOneWayAjarProtocol,
			serversuite.TestUnknownFlexibleOneWayAjarProtocol,
			serversuite.TestUnknownStrictTwoWayAjarProtocol,
			serversuite.TestUnknownFlexibleTwoWayAjarProtocol,
			serversuite.TestUnknownStrictOneWayClosedProtocol,
			serversuite.TestUnknownFlexibleOneWayClosedProtocol,
			serversuite.TestUnknownStrictTwoWayClosedProtocol,
			serversuite.TestUnknownFlexibleTwoWayClosedProtocol:
			return false

		case serversuite.TestV1TwoWayNoPayload, serversuite.TestV1TwoWayStructPayload:
			// TODO(fxbug.dev/99738): Go bindings should reject V1 wire format.
			return false

		case serversuite.TestServerTearsDownWhenPeerClosed:
			// TODO(fxbug.dev/120781): Report PEER_CLOSED through WillTeardown.
			return false

		case serversuite.TestEventSendingDoNotReportPeerClosed, serversuite.TestReplySendingDoNotReportPeerClosed:
			// TODO(fxbug.dev/113160): Peer-closed errors should be
			// hidden from one-way calls.
			return false

		default:
			return true
		}
	}
	return isEnabled(test), nil
}

func (*runnerImpl) IsTeardownReasonSupported(_ fidl.Context) (bool, error) {
	return false, nil
}

func (*runnerImpl) Start(_ fidl.Context, target serversuite.AnyTarget) error {
	if target.Which() != serversuite.AnyTargetClosedTarget {
		return errors.New("Go only supports closed protocols")
	}

	controllerChannel := target.ClosedTarget.Controller.Channel
	sutChannel := target.ClosedTarget.Sut.Channel
	go func() {
		stub := serversuite.ClosedTargetControllerWithCtxStub{
			Impl: &closedTargetControllerImpl{
				sutChannel: sutChannel,
			},
		}
		component.Serve(context.Background(), &stub, controllerChannel, component.ServeOptions{
			OnError: func(err error) {
				_ = syslog.WarnTf("server_suite/go", "%s", err)
			},
		})
	}()
	go func() {
		controllerEventProxy := serversuite.ClosedTargetControllerEventProxy(fidl.ChannelProxy{Channel: controllerChannel})
		stub := serversuite.ClosedTargetWithCtxStub{
			Impl: &closedTargetImpl{
				controllerEventProxy: controllerEventProxy,
			},
		}
		component.Serve(context.Background(), &stub, sutChannel, component.ServeOptions{
			OnError: func(err error) {
				// Failures are expected as part of tests.
				log.Printf("serversuite.ClosedTarget errored: %s", err)
				controllerEventProxy.WillTeardown(serversuite.TeardownReasonOther)
			},
		})
	}()

	return nil
}

func (*runnerImpl) CheckAlive(_ fidl.Context) error { return nil }

func main() {
	log.SetFlags(log.Lshortfile)

	log.Println("Go serversuite server: starting")
	ctx := component.NewContextFromStartupInfo()
	ctx.OutgoingService.AddService(
		serversuite.RunnerName,
		func(ctx context.Context, c zx.Channel) error {
			stub := serversuite.RunnerWithCtxStub{
				Impl: &runnerImpl{},
			}
			go component.Serve(ctx, &stub, c, component.ServeOptions{
				OnError: func(err error) {
					// Panic because the test runner should never fail.
					panic(fmt.Sprintf("serversuite.Runner errored: %s", err))
				},
			})
			return nil
		},
	)
	log.Println("Go serversuite server: ready")
	ctx.BindStartupHandle(context.Background())
}
