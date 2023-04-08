// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package readme

import (
	"testing"

	"github.com/google/go-cmp/cmp"
)

func TestBuilder(t *testing.T) {
	want := `` +
		"Name: foo\n" +
		"URL: www.foo.bar\n" +
		"License File: license\n" +
		"License File URL: www.foo.bar/license\n" +
		"License File Format: Single License File\n"

	var b builder
	b.setName("foo")
	b.setPath("grandparent/parent/dir/testproject")
	b.setURL("www.foo.bar")
	b.addLicense("license", "www.foo.bar/license", singleLicenseFile)
	got := b.build()

	if diff := cmp.Diff(want, got); diff != "" {
		t.Errorf("%s: compare readme text mismatch: (-want +got):\n%s", t.Name(), diff)
	}
}
