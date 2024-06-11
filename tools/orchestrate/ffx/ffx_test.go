// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
package orchestrate

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestFfxEnvContainsSsh(t *testing.T) {
	ffx := &Ffx{Dir: "/foo/bar", bin: "foo/bar/ffx", sslCertPath: "/this/or/something"}
	cmd, err := ffx.Cmd("config", "get")
	if err != nil {
		t.Error(err)
	}
	pathFound := false
	wd, err := os.Getwd()
	if err != nil {
		t.Error(err)
	}
	for _, v := range cmd.Env {
		if strings.HasPrefix(v, "PATH=") && strings.Contains(v, filepath.Join(wd, "openssh-portable", "bin")) {
			pathFound = true
		}
	}
	if !pathFound {
		t.Errorf("SSH not found in env: %v", cmd.Env)
	}
}
