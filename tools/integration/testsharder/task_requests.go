// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package testsharder

import (
	"strings"

	"go.fuchsia.dev/fuchsia/tools/integration/testsharder/proto"
)

func GetBotDimensions(shard *Shard, params *proto.Params) {
	dimensions := map[string]string{"pool": params.Pool}
	isEmuType := shard.Env.TargetsEmulator()
	testBotCpu := shard.HostCPU(params.UseTcg)

	isLinux := shard.Env.Dimensions.OS() == "" || strings.ToLower(shard.Env.Dimensions.OS()) == "linux"
	isGCEType := shard.Env.Dimensions.DeviceType() == "GCE"

	if isEmuType {
		dimensions["os"] = "Debian"
		if !params.UseTcg {
			dimensions["kvm"] = "1"
		}
		dimensions["cpu"] = testBotCpu
	} else if isGCEType {
		// Have any GCE shards target the GCE executors, which are e2-2
		// machines running Linux.
		dimensions["os"] = "Linux"
		dimensions["cores"] = "2"
		dimensions["gce"] = "1"
	} else {
		// No device -> no serial.
		if !shard.ExpectsSSH && shard.Env.Dimensions.DeviceType() != "" {
			dimensions["serial"] = "1"
		}
		for k, v := range shard.Env.Dimensions {
			dimensions[k] = v
		}
	}
	// Ensure we use GCE VMs whenever possible.
	if isLinux && !isGCEType && testBotCpu == "x64" && !params.UseTcg {
		dimensions["kvm"] = "1"
	}
	if (isEmuType || shard.Env.Dimensions.DeviceType() == "") && testBotCpu == "x64" && isLinux {
		dimensions["gce"] = "1"
		dimensions["cores"] = "8"
	}
	shard.BotDimensions = dimensions
}
