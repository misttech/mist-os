// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//go:build !build_with_native_toolchain

package netstack

import (
	"context"
	"syscall/zx/fidl"
	"testing"

	"go.fuchsia.dev/fuchsia/src/lib/component"
	syslog "go.fuchsia.dev/fuchsia/src/lib/syslog/go"

	"fidl/fuchsia/diagnostics/persist"
	"fidl/fuchsia/net/neighbor"

	"gvisor.dev/gvisor/pkg/tcpip/faketime"
)

var _ persist.DataPersistenceWithCtx = (*fakePersistServer)(nil)

type fakePersistServer struct {
	requests []string
}

func (server *fakePersistServer) Persist(ctx fidl.Context, tag string) (persist.PersistResult, error) {
	server.requests = append(server.requests, tag)
	return persist.PersistResultQueued, nil
}

func (server *fakePersistServer) ServeInBackground(t *testing.T, req persist.DataPersistenceWithCtxInterfaceRequest, ctx context.Context) chan struct{} {
	stub := persist.DataPersistenceWithCtxStub{Impl: server}
	serveDone := make(chan struct{})
	go func() {
		component.Serve(ctx, &stub, req.Channel, component.ServeOptions{
			Concurrent: true,
			OnError: func(err error) {
				t.Errorf("got unexpected error %s", err)
				_ = syslog.WarnTf(neighbor.ViewName, "%s", err)
			},
		})
		close(serveDone)
	}()
	return serveDone
}

func TestPeriodicallyRequestsPersistence(t *testing.T) {
	req, client, err := persist.NewDataPersistenceWithCtxInterfaceRequest()
	if err != nil {
		t.Fatalf("persist.NewDataPersistenceWithCtxInterfaceRequest(): %s", err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	impl := fakePersistServer{}
	serveDone := impl.ServeInBackground(t, req, ctx)
	defer func() {
		cancel()
		<-serveDone
	}()

	// Use a different context for the client so it can be cancelled before the
	// server.
	ctx, clientCancel := context.WithCancel(context.Background())
	clock := faketime.NewManualClock()

	runPersistClient(client, ctx, clock)
	expectRequests(t, impl.requests, len(tags))

	clock.Advance(3 * persistPeriod)
	expectRequests(t, impl.requests, 4*len(tags))

	clientCancel()
	clock.Advance(2 * persistPeriod)
	expectRequests(t, impl.requests, 4*len(tags))
}

func TestExitsAfterRequestChannelFailure(t *testing.T) {
	req, client, err := persist.NewDataPersistenceWithCtxInterfaceRequest()
	if err != nil {
		t.Fatalf("persist.NewDataPersistenceWithCtxInterfaceRequest(): %s", err)
	}

	impl := fakePersistServer{}
	var serveCancelAndWait context.CancelFunc
	{
		ctx, cancel := context.WithCancel(context.Background())
		serveDone := impl.ServeInBackground(t, req, ctx)
		serveCancelAndWait = func() {
			cancel()
			<-serveDone
		}
	}
	defer serveCancelAndWait()

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	clock := faketime.NewManualClock()

	runPersistClient(client, ctx, clock)
	expectRequests(t, impl.requests, len(tags))

	// Cancel the server and wait for it to shut down.
	serveCancelAndWait()

	clock.Advance(persistPeriod)
	expectRequests(t, impl.requests, len(tags))

	clock.Advance(persistPeriod)
	expectRequests(t, impl.requests, len(tags))
}

func expectRequests(t *testing.T, requests []string, want int) {
	t.Helper()
	if got := len(requests); got != want {
		t.Errorf("len(requests) = %d, want %d", got, want)
	}

	for i := 0; i < len(requests); i++ {
		if got, want := requests[i], tags[i%len(tags)]; got != want {
			t.Errorf("requests[%d] = %s, want %s", i, got, want)
		}
	}
}
