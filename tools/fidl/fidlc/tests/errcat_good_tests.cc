// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zxtest/zxtest.h>

#include "tools/fidl/fidlc/tests/error_test.h"
#include "tools/fidl/fidlc/tests/test_library.h"

// This file is meant to hold standalone tests for each of the "good" examples used in the documents
// at //docs/reference/fidl/language/error-catalog. These cases are redundant with the other tests
// in this suite - their purpose is not to serve as tests for the features at hand, but rather to
// provide well-vetted and tested examples of the "correct" way to fix FIDL errors.

namespace {

TEST(ErrcatGoodTests, Good0001) {
  TestLibrary library;
  library.AddFile("good/fi-0001.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0002) {
  TestLibrary library;
  library.AddFile("good/fi-0002.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0003) {
  TestLibrary library;
  library.AddFile("good/fi-0003.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0004) {
  TestLibrary library;
  library.AddFile("good/fi-0004.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0006) {
  TestLibrary library;
  library.AddFile("good/fi-0006.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0007) {
  TestLibrary library;
  library.AddFile("good/fi-0007.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0008) {
  TestLibrary library;
  library.AddFile("good/fi-0008.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0009) {
  TestLibrary library;
  library.AddFile("good/fi-0009.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0010a) {
  TestLibrary library;
  library.AddFile("good/fi-0010-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0010b) {
  TestLibrary library;
  library.AddFile("good/fi-0010-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0011) {
  TestLibrary library;
  library.AddFile("good/fi-0011.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0012) {
  TestLibrary library;
  library.AddFile("good/fi-0012.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0013) {
  TestLibrary library;
  library.AddFile("good/fi-0013.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0014) {
  TestLibrary library;
  library.AddFile("good/fi-0014.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0015) {
  TestLibrary library;
  library.AddFile("good/fi-0015.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0016) {
  TestLibrary library;
  library.AddFile("good/fi-0016.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0017) {
  TestLibrary library;
  library.AddFile("good/fi-0017.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0018) {
  TestLibrary library;
  library.AddFile("good/fi-0018.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0019a) {
  TestLibrary library;
  library.AddFile("good/fi-0019-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0019b) {
  TestLibrary library;
  library.AddFile("good/fi-0019-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0020) {
  TestLibrary library;
  library.AddFile("good/fi-0020.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0022) {
  TestLibrary library;
  library.AddFile("good/fi-0022.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0023) {
  TestLibrary library;
  library.AddFile("good/fi-0023.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0024) {
  TestLibrary library;
  library.AddFile("good/fi-0024.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0025) {
  SharedAmongstLibraries shared;
  TestLibrary dependency(&shared, "dependent.fidl", R"FIDL(library dependent;

type Something = struct {};
)FIDL");
  ASSERT_COMPILED(dependency);
  TestLibrary library(&shared);
  library.AddFile("good/fi-0025.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0026) {
  TestLibrary library;
  library.AddFile("good/fi-0026.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0027a) {
  TestLibrary library;
  library.AddFile("good/fi-0027-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0027b) {
  TestLibrary library;
  library.AddFile("good/fi-0027-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0028a) {
  TestLibrary library;
  library.AddFile("good/fi-0028-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0028b) {
  TestLibrary library;
  library.AddFile("good/fi-0028-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0029) {
  TestLibrary library;
  library.AddFile("good/fi-0029.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0030) {
  TestLibrary library;
  library.AddFile("good/fi-0030.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0031) {
  TestLibrary library;
  library.AddFile("good/fi-0031.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0032) {
  TestLibrary library;
  library.AddFile("good/fi-0032.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0033) {
  TestLibrary library;
  library.AddFile("good/fi-0033.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0034a) {
  TestLibrary library;
  library.AddFile("good/fi-0034-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0034b) {
  TestLibrary library;
  library.AddFile("good/fi-0034-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0035) {
  TestLibrary library;
  library.AddFile("good/fi-0035.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0036) {
  TestLibrary library;
  library.AddFile("good/fi-0036.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0037) {
  TestLibrary library;
  library.AddFile("good/fi-0037.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0038ab) {
  SharedAmongstLibraries shared;
  TestLibrary dependency(&shared);
  dependency.AddFile("good/fi-0038-a.test.fidl");
  ASSERT_COMPILED(dependency);
  TestLibrary library(&shared);
  library.AddFile("good/fi-0038-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0038ac) {
  SharedAmongstLibraries shared;
  TestLibrary dependency(&shared);
  dependency.AddFile("good/fi-0038-a.test.fidl");
  ASSERT_COMPILED(dependency);
  TestLibrary library(&shared);
  library.AddFile("good/fi-0038-c.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0039ab) {
  SharedAmongstLibraries shared;
  TestLibrary dependency(&shared);
  dependency.AddFile("good/fi-0039-a.test.fidl");
  ASSERT_COMPILED(dependency);
  TestLibrary library(&shared);
  library.AddFile("good/fi-0039-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0039ac) {
  SharedAmongstLibraries shared;
  TestLibrary dependency(&shared);
  dependency.AddFile("good/fi-0039-a.test.fidl");
  ASSERT_COMPILED(dependency);
  TestLibrary library(&shared);
  library.AddFile("good/fi-0039-c.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0040) {
  TestLibrary library;
  library.AddFile("good/fi-0040-a.test.fidl");
  library.AddFile("good/fi-0040-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0041a) {
  TestLibrary library;
  library.AddFile("good/fi-0041-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0041b) {
  TestLibrary library;
  library.AddFile("good/fi-0041-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0042) {
  SharedAmongstLibraries shared;
  TestLibrary dependency(&shared);
  dependency.AddFile("good/fi-0042-a.test.fidl");
  ASSERT_COMPILED(dependency);
  TestLibrary library(&shared);
  library.AddFile("good/fi-0042-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0043) {
  SharedAmongstLibraries shared;
  TestLibrary dependency1(&shared);
  dependency1.AddFile("good/fi-0043-a.test.fidl");
  ASSERT_COMPILED(dependency1);
  TestLibrary dependency2(&shared);
  dependency2.AddFile("good/fi-0043-b.test.fidl");
  ASSERT_COMPILED(dependency2);
  TestLibrary library(&shared);
  library.AddFile("good/fi-0043-c.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0044) {
  SharedAmongstLibraries shared;
  TestLibrary dependency1(&shared);
  dependency1.AddFile("good/fi-0044-a.test.fidl");
  ASSERT_COMPILED(dependency1);
  TestLibrary dependency2(&shared);
  dependency2.AddFile("good/fi-0044-b.test.fidl");
  ASSERT_COMPILED(dependency2);
  TestLibrary library(&shared);
  library.AddFile("good/fi-0044-c.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0045) {
  SharedAmongstLibraries shared;
  TestLibrary dependency(&shared);
  dependency.AddFile("good/fi-0045-a.test.fidl");
  ASSERT_COMPILED(dependency);
  TestLibrary library(&shared);
  library.AddFile("good/fi-0045-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0046) {
  TestLibrary library;
  library.AddFile("good/fi-0046.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0047) {
  TestLibrary library;
  library.AddFile("good/fi-0047.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0048) {
  TestLibrary library;
  library.AddFile("good/fi-0048.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0049) {
  TestLibrary library;
  library.AddFile("good/fi-0049.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0050) {
  TestLibrary library;
  library.AddFile("good/fi-0050.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0051) {
  SharedAmongstLibraries shared;
  TestLibrary dependency(&shared);
  dependency.AddFile("good/fi-0051-a.test.fidl");
  ASSERT_COMPILED(dependency);
  TestLibrary library(&shared);
  library.AddFile("good/fi-0051-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0052a) {
  TestLibrary library;
  library.AddFile("good/fi-0052-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0052b) {
  TestLibrary library;
  library.AddFile("good/fi-0052-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0053a) {
  TestLibrary library;
  library.AddFile("good/fi-0053-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0053b) {
  TestLibrary library;
  library.AddFile("good/fi-0053-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0054) {
  TestLibrary library;
  library.AddFile("good/fi-0054.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0055) {
  TestLibrary library;
  library.AddFile("good/fi-0055.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0056) {
  SharedAmongstLibraries shared;
  TestLibrary dependency(&shared);
  dependency.AddFile("good/fi-0056-a.test.fidl");
  ASSERT_COMPILED(dependency);
  TestLibrary library(&shared);
  library.AddFile("good/fi-0056-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(StructsTests, Good0057) {
  TestLibrary library;
  library.AddFile("good/fi-0057.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0058) {
  TestLibrary library;
  library.AddFile("good/fi-0058.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0059) {
  TestLibrary library;
  library.AddFile("good/fi-0059.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0060) {
  TestLibrary library;
  library.AddFile("good/fi-0060.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0061) {
  TestLibrary library;
  library.AddFile("good/fi-0061.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0062a) {
  TestLibrary library;
  library.AddFile("good/fi-0062-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0062b) {
  TestLibrary library;
  library.AddFile("good/fi-0062-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0063) {
  TestLibrary library;
  library.AddFile("good/fi-0063.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0064a) {
  TestLibrary library;
  library.AddFile("good/fi-0064-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0064b) {
  TestLibrary library;
  library.AddFile("good/fi-0064-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0065a) {
  TestLibrary library;
  library.AddFile("good/fi-0065-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0065b) {
  TestLibrary library;
  library.AddFile("good/fi-0065-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0065c) {
  TestLibrary library;
  library.AddFile("good/fi-0065-c.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0066a) {
  TestLibrary library;
  library.AddFile("good/fi-0066-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0066b) {
  TestLibrary library;
  library.AddFile("good/fi-0066-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0067a) {
  TestLibrary library;
  library.AddFile("good/fi-0067-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0067b) {
  TestLibrary library;
  library.AddFile("good/fi-0067-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0068a) {
  TestLibrary library;
  library.AddFile("good/fi-0068-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0068b) {
  TestLibrary library;
  library.AddFile("good/fi-0068-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0069) {
  TestLibrary library;
  library.AddFile("good/fi-0069.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0070) {
  TestLibrary library;
  library.AddFile("good/fi-0070.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0071a) {
  TestLibrary library;
  library.AddFile("good/fi-0071-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0071b) {
  TestLibrary library;
  library.AddFile("good/fi-0071-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0072a) {
  TestLibrary library;
  library.AddFile("good/fi-0072-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0072b) {
  TestLibrary library;
  library.AddFile("good/fi-0072-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0073) {
  TestLibrary library;
  library.AddFile("good/fi-0073.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0074) {
  TestLibrary library;
  library.AddFile("good/fi-0074.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0075) {
  TestLibrary library;
  library.AddFile("good/fi-0075.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0077a) {
  TestLibrary library;
  library.AddFile("good/fi-0077-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0077b) {
  TestLibrary library;
  library.AddFile("good/fi-0077-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0078a) {
  TestLibrary library;
  library.AddFile("good/fi-0078-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0078b) {
  TestLibrary library;
  library.AddFile("good/fi-0078-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0079) {
  TestLibrary library;
  library.AddFile("good/fi-0079.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0081) {
  TestLibrary library;
  library.AddFile("good/fi-0081.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0082) {
  TestLibrary library;
  library.AddFile("good/fi-0082.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0083) {
  TestLibrary library;
  library.AddFile("good/fi-0083.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0084) {
  TestLibrary library;
  library.AddFile("good/fi-0084.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0085a) {
  TestLibrary library;
  library.AddFile("good/fi-0085-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0085b) {
  TestLibrary library;
  library.AddFile("good/fi-0085-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0086a) {
  TestLibrary library;
  library.AddFile("good/fi-0086-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0086b) {
  TestLibrary library;
  library.AddFile("good/fi-0086-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0087) {
  TestLibrary library;
  library.AddFile("good/fi-0087.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0088) {
  TestLibrary library;
  library.AddFile("good/fi-0088.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0089a) {
  TestLibrary library;
  library.AddFile("good/fi-0089-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0089b) {
  TestLibrary library;
  library.AddFile("good/fi-0089-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0090) {
  TestLibrary library;
  library.AddFile("good/fi-0090.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0091) {
  TestLibrary library;
  library.AddFile("good/fi-0091.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0092) {
  TestLibrary library;
  library.AddFile("good/fi-0092.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0093) {
  TestLibrary library;
  library.AddFile("good/fi-0093.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0094a) {
  TestLibrary library;
  library.AddFile("good/fi-0094-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0094b) {
  TestLibrary library;
  library.AddFile("good/fi-0094-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0095a) {
  TestLibrary library;
  library.AddFile("good/fi-0095-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0095b) {
  TestLibrary library;
  library.AddFile("good/fi-0095-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0096a) {
  TestLibrary library;
  library.AddFile("good/fi-0096-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0096b) {
  TestLibrary library;
  library.AddFile("good/fi-0096-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0097a) {
  TestLibrary library;
  library.AddFile("good/fi-0097-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0097b) {
  TestLibrary library;
  library.AddFile("good/fi-0097-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0098a) {
  TestLibrary library;
  library.AddFile("good/fi-0098-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0098b) {
  TestLibrary library;
  library.AddFile("good/fi-0098-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0099a) {
  TestLibrary library;
  library.AddFile("good/fi-0099-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0099b) {
  TestLibrary library;
  library.AddFile("good/fi-0099-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0100a) {
  TestLibrary library;
  library.AddFile("good/fi-0100-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0100b) {
  TestLibrary library;
  library.AddFile("good/fi-0100-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0101) {
  TestLibrary library;
  library.AddFile("good/fi-0101.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0102) {
  TestLibrary library;
  library.AddFile("good/fi-0102.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0103) {
  TestLibrary library;
  library.AddFile("good/fi-0103.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0104) {
  TestLibrary library;
  library.AddFile("good/fi-0104.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0105a) {
  TestLibrary library;
  library.AddFile("good/fi-0105-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0105b) {
  TestLibrary library;
  library.AddFile("good/fi-0105-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0106) {
  TestLibrary library;
  library.AddFile("good/fi-0106.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0107a) {
  TestLibrary library;
  library.AddFile("good/fi-0107-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0107b) {
  TestLibrary library;
  library.AddFile("good/fi-0107-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0108) {
  TestLibrary library;
  library.AddFile("good/fi-0108.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0109) {
  TestLibrary library;
  library.AddFile("good/fi-0109.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0110a) {
  TestLibrary library;
  library.AddFile("good/fi-0110-a.test.fidl");
  library.UseLibraryZx();
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0110b) {
  TestLibrary library;
  library.AddFile("good/fi-0110-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0111) {
  TestLibrary library;
  library.AddFile("good/fi-0111.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0112) {
  TestLibrary library;
  library.AddFile("good/fi-0112.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0113) {
  TestLibrary library;
  library.AddFile("good/fi-0113.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0114a) {
  TestLibrary library;
  library.AddFile("good/fi-0114-a.test.fidl");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kUnknownInteractions);
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kUnknownInteractionsNewDefaults);
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0114b) {
  TestLibrary library;
  library.AddFile("good/fi-0114-b.test.fidl");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kUnknownInteractions);
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kUnknownInteractionsNewDefaults);
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0115a) {
  TestLibrary library;
  library.AddFile("good/fi-0115-a.test.fidl");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kUnknownInteractions);
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kUnknownInteractionsNewDefaults);
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0115b) {
  TestLibrary library;
  library.AddFile("good/fi-0115-b.test.fidl");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kUnknownInteractions);
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kUnknownInteractionsNewDefaults);
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0116a) {
  TestLibrary library;
  library.AddFile("good/fi-0116-a.test.fidl");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kUnknownInteractions);
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kUnknownInteractionsNewDefaults);
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0116b) {
  TestLibrary library;
  library.AddFile("good/fi-0116-b.test.fidl");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kUnknownInteractions);
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kUnknownInteractionsNewDefaults);
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0117a) {
  TestLibrary library;
  library.AddFile("good/fi-0117-a.test.fidl");
  library.UseLibraryZx();
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0117b) {
  TestLibrary library;
  library.AddFile("good/fi-0117-b.test.fidl");
  library.UseLibraryFdf();
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0118) {
  TestLibrary library;
  library.AddFile("good/fi-0118.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0119a) {
  TestLibrary library;
  library.AddFile("good/fi-0119-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0119b) {
  TestLibrary library;
  library.AddFile("good/fi-0119-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0120a) {
  TestLibrary library;
  library.AddFile("good/fi-0120-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0120b) {
  TestLibrary library;
  library.AddFile("good/fi-0120-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0121) {
  TestLibrary library;
  library.AddFile("good/fi-0121.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0122) {
  TestLibrary library;
  library.AddFile("good/fi-0122.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0123) {
  TestLibrary library;
  library.AddFile("good/fi-0123.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0124) {
  TestLibrary library;
  library.AddFile("good/fi-0124.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0125) {
  TestLibrary library;
  library.AddFile("good/fi-0125.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0126) {
  TestLibrary library;
  library.AddFile("good/fi-0126.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0127) {
  TestLibrary library;
  library.AddFile("good/fi-0127.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0128) {
  TestLibrary library;
  library.AddFile("good/fi-0128.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0129a) {
  TestLibrary library;
  library.AddFile("good/fi-0129-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0129b) {
  TestLibrary library;
  library.AddFile("good/fi-0129-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0130) {
  TestLibrary library;
  library.AddFile("good/fi-0130.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0131a) {
  TestLibrary library;
  library.AddFile("good/fi-0131-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0131b) {
  TestLibrary library;
  library.AddFile("good/fi-0131-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0132) {
  TestLibrary library;
  library.AddFile("good/fi-0132.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0133) {
  TestLibrary library;
  library.AddFile("good/fi-0133.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0135) {
  TestLibrary library;
  library.AddFile("good/fi-0135.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0141) {
  TestLibrary library;
  library.AddFile("good/fi-0141.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0142) {
  TestLibrary library;
  library.AddFile("good/fi-0142.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0145) {
  TestLibrary library;
  library.AddFile("good/fi-0145.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0146) {
  TestLibrary library;
  library.AddFile("good/fi-0146.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0147) {
  TestLibrary library;
  library.AddFile("good/fi-0147.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0148a) {
  TestLibrary library;
  library.AddFile("good/fi-0148-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0148b) {
  TestLibrary library;
  library.AddFile("good/fi-0148-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0149a) {
  TestLibrary library;
  library.AddFile("good/fi-0149-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0149b) {
  TestLibrary library;
  library.AddFile("good/fi-0149-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0150a) {
  TestLibrary library;
  library.AddFile("good/fi-0150-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0150b) {
  TestLibrary library;
  library.AddFile("good/fi-0150-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0151a) {
  TestLibrary library;
  library.AddFile("good/fi-0151-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0151b) {
  TestLibrary library;
  library.AddFile("good/fi-0151-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0152) {
  TestLibrary library;
  library.AddFile("good/fi-0152.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0153) {
  TestLibrary library;
  library.AddFile("good/fi-0153.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0154a) {
  TestLibrary library;
  library.AddFile("good/fi-0154-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0154b) {
  TestLibrary library;
  library.AddFile("good/fi-0154-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0155) {
  TestLibrary library;
  library.AddFile("good/fi-0155.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0156) {
  TestLibrary library;
  library.AddFile("good/fi-0156.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0157) {
  TestLibrary library;
  library.AddFile("good/fi-0157.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0158) {
  TestLibrary library;
  library.AddFile("good/fi-0158.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0159) {
  TestLibrary library;
  library.AddFile("good/fi-0159.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0160a) {
  TestLibrary library;
  library.AddFile("good/fi-0160-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0160b) {
  TestLibrary library;
  library.AddFile("good/fi-0160-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0161) {
  TestLibrary library;
  library.AddFile("good/fi-0161.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0162) {
  TestLibrary library;
  library.AddFile("good/fi-0162.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0163) {
  TestLibrary library;
  library.AddFile("good/fi-0163.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0164) {
  TestLibrary library;
  library.AddFile("good/fi-0164.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0165) {
  TestLibrary library;
  library.AddFile("good/fi-0165.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0166) {
  TestLibrary library;
  library.AddFile("good/fi-0166.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0167) {
  TestLibrary library;
  library.AddFile("good/fi-0167.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0168) {
  TestLibrary library;
  library.AddFile("good/fi-0168.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0169) {
  TestLibrary library;
  library.AddFile("good/fi-0169.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0170) {
  TestLibrary library;
  library.AddFile("good/fi-0170.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0171) {
  TestLibrary library;
  library.AddFile("good/fi-0171.test.fidl");
  library.UseLibraryZx();
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0172) {
  TestLibrary library;
  library.AddFile("good/fi-0172.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0173) {
  TestLibrary library;
  library.AddFile("good/fi-0173.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0175) {
  TestLibrary library;
  library.AddFile("good/fi-0175.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0177) {
  TestLibrary library;
  library.AddFile("good/fi-0177.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0178) {
  SharedAmongstLibraries shared;
  TestLibrary dependency(&shared, "dependent.fidl", R"FIDL(library dependent;
type Bar = struct {};
)FIDL");
  ASSERT_COMPILED(dependency);
  TestLibrary library(&shared);
  library.AddFile("good/fi-0178.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0179) {
  TestLibrary library;
  library.AddFile("good/fi-0179.test.fidl");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kAllowNewTypes);
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0180) {
  TestLibrary library;
  library.AddFile("good/fi-0180.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0181) {
  TestLibrary library;
  library.AddFile("good/fi-0181.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0182a) {
  TestLibrary library;
  library.AddFile("good/fi-0182-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0182b) {
  TestLibrary library;
  library.AddFile("good/fi-0182-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0183a) {
  TestLibrary library;
  library.AddFile("good/fi-0183-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0183b) {
  TestLibrary library;
  library.AddFile("good/fi-0183-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0184a) {
  TestLibrary library;
  library.AddFile("good/fi-0184-a.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0184b) {
  TestLibrary library;
  library.AddFile("good/fi-0184-b.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0185) {
  TestLibrary library;
  library.AddFile("good/fi-0185.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0186) {
  TestLibrary library;
  library.AddFile("good/fi-0186.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0187) {
  TestLibrary library;
  library.AddFile("good/fi-0187.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0188) {
  TestLibrary library;
  library.AddFile("good/fi-0188.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0189) {
  TestLibrary library;
  library.AddFile("good/fi-0189.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0191) {
  TestLibrary library;
  library.AddFile("good/fi-0191.test.fidl");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kUnknownInteractions);
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kUnknownInteractionsMandate);
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0192) {
  TestLibrary library;
  library.AddFile("good/fi-0192.test.fidl");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kUnknownInteractions);
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kUnknownInteractionsMandate);
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0193) {
  TestLibrary library;
  library.AddFile("good/fi-0193.test.fidl");
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0194a) {
  TestLibrary library;
  library.AddFile("good/fi-0194-a.test.fidl");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kUnknownInteractions);
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kUnknownInteractionsMandate);
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kSimpleEmptyResponseSyntax);
  ASSERT_COMPILED(library);
}

TEST(ErrcatGoodTests, Good0194b) {
  TestLibrary library;
  library.AddFile("good/fi-0194-b.test.fidl");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kUnknownInteractions);
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kUnknownInteractionsMandate);
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kSimpleEmptyResponseSyntax);
  ASSERT_COMPILED(library);
}

// TODO(fxbug.dev/100478): Retire once large messages no longer requires flag or attribute.
TEST(ErrcatGoodTests, Good0195) {
  TestLibrary library;
  library.AddFile("good/fi-0195.test.fidl");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kAllowOverflowing);
  ASSERT_COMPILED(library);
}

// TODO(fxbug.dev/100478): Retire once large messages no longer requires flag or attribute.
TEST(ErrcatGoodTests, Good0196a) {
  TestLibrary library;
  library.AddFile("good/fi-0196-a.test.fidl");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kAllowOverflowing);
  ASSERT_COMPILED(library);
}

// TODO(fxbug.dev/100478): Retire once large messages no longer requires flag or attribute.
TEST(ErrcatGoodTests, Good0196b) {
  TestLibrary library;
  library.AddFile("good/fi-0196-b.test.fidl");
  library.EnableFlag(fidl::ExperimentalFlags::Flag::kAllowOverflowing);
  ASSERT_COMPILED(library);
}

}  // namespace
