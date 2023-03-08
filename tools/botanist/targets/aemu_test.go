// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package targets

import (
	"context"
	"testing"
)

func TestNewAEMU(t *testing.T) {
	ctx := context.Background()
	a, err := NewAEMU(
		ctx,
		QEMUConfig{
			Target: "x64",
		},
		Options{},
	)
	if err != nil {
		t.Fatalf("Unable to create NewAEMU: %s", err)
	}

	if a.binary != aemuBinaryName {
		t.Errorf("Unexpected aemu binary %s, expected %s", a.binary, aemuBinaryName)
	}
}
