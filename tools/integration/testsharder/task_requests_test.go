// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package testsharder

import (
	"testing"

	"github.com/google/go-cmp/cmp"

	"go.fuchsia.dev/fuchsia/tools/build"
	"go.fuchsia.dev/fuchsia/tools/integration/testsharder/proto"
)

func TestGetBotDimensions(t *testing.T) {
	x64EmuEnv := build.Environment{
		Dimensions: build.DimensionSet{"device_type": "QEMU", "cpu": "x64"},
	}
	arm64EmuEnv := build.Environment{
		Dimensions: build.DimensionSet{"device_type": "QEMU", "cpu": "arm64"},
	}
	nucEnv := build.Environment{
		Dimensions: build.DimensionSet{"device_type": "NUC"},
	}
	linuxEnv := build.Environment{
		Dimensions: build.DimensionSet{"os": "Linux", "cpu": "x64"},
	}
	gceEnv := build.Environment{
		Dimensions: build.DimensionSet{"device_type": "GCE"},
	}
	macEnv := build.Environment{
		Dimensions: build.DimensionSet{"os": "Mac", "cpu": "x64"},
	}

	testCases := []struct {
		name       string
		env        build.Environment
		os         string
		cpu        string
		expectsSSH bool
		params     *proto.Params
		want       map[string]string
	}{
		{
			name:       "x64 emulator with ssh",
			env:        x64EmuEnv,
			cpu:        "x64",
			expectsSSH: true,
			params: &proto.Params{
				Pool: "pool",
			},
			want: map[string]string{"pool": "pool", "kvm": "1", "os": "Debian", "gce": "1", "cores": "8", "cpu": "x64"},
		},
		{
			name: "x64 emulator without ssh",
			env:  x64EmuEnv,
			cpu:  "x64",
			params: &proto.Params{
				Pool: "pool",
			},
			want: map[string]string{"pool": "pool", "kvm": "1", "os": "Debian", "gce": "1", "cores": "8", "cpu": "x64"},
		},
		{
			name:       "arm64 emulator with ssh",
			env:        arm64EmuEnv,
			cpu:        "arm64",
			expectsSSH: true,
			params: &proto.Params{
				Pool: "pool",
			},
			want: map[string]string{"pool": "pool", "kvm": "1", "os": "Debian", "cpu": "arm64"},
		},
		{
			name: "arm64 emulator without ssh",
			env:  arm64EmuEnv,
			cpu:  "arm64",
			params: &proto.Params{
				Pool: "pool",
			},
			want: map[string]string{"pool": "pool", "kvm": "1", "os": "Debian", "cpu": "arm64"},
		},
		{
			name:       "arm64 emulator tcg",
			env:        arm64EmuEnv,
			cpu:        "arm64",
			expectsSSH: true,
			params: &proto.Params{
				Pool:   "pool",
				UseTcg: true,
			},
			want: map[string]string{"pool": "pool", "os": "Debian", "gce": "1", "cores": "8", "cpu": "x64"},
		},
		{
			name:       "device with ssh",
			env:        nucEnv,
			cpu:        "x64",
			expectsSSH: true,
			params: &proto.Params{
				Pool: "pool",
			},
			want: map[string]string{"pool": "pool", "kvm": "1", "device_type": "NUC"},
		},
		{
			name: "device without ssh",
			env:  nucEnv,
			cpu:  "x64",
			params: &proto.Params{
				Pool: "pool",
			},
			want: map[string]string{"pool": "pool", "kvm": "1", "serial": "1", "device_type": "NUC"},
		},
		{
			name:       "GCE",
			env:        gceEnv,
			cpu:        "x64",
			expectsSSH: true,
			params: &proto.Params{
				Pool: "pool",
			},
			want: map[string]string{"pool": "pool", "os": "Linux", "gce": "1", "cores": "2"},
		},
		{
			name: "linux",
			env:  linuxEnv,
			cpu:  "x64",
			params: &proto.Params{
				Pool: "pool",
			},
			want: map[string]string{"pool": "pool", "kvm": "1", "os": "Linux", "gce": "1", "cores": "8", "cpu": "x64"},
		},
		{
			name: "mac",
			env:  macEnv,
			cpu:  "x64",
			params: &proto.Params{
				Pool: "pool",
			},
			want: map[string]string{"pool": "pool", "os": "Mac", "cpu": "x64"},
		},
	}
	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			test := makeTest(1, tc.os)
			test.CPU = tc.cpu
			shard := &Shard{
				Name:       environmentName(tc.env),
				Tests:      []Test{test},
				Env:        tc.env,
				ExpectsSSH: tc.expectsSSH,
			}
			GetBotDimensions(shard, tc.params)
			if diff := cmp.Diff(tc.want, shard.BotDimensions); diff != "" {
				t.Errorf("Wrong bot dimensions (-want +got):\n%s", diff)
			}
		})
	}
}
