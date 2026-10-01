// Prints [[input, message], ...]: the SyntaxError message JSON.parse throws
// for each input. Run with the pinned node 22.20.0:
//   node tests/oracle/v8-json-errors.mjs > tests/fixtures/v8-json-errors.json
const cases = [
  "", " ", "{", "[", "{bad", "{\"a\"", "{\"a\":", "{\"a\":1", "{\"a\":1,", "{\"a\" 1}",
  "[1", "[1,", "[1 2]", "[1,]", "{\"a\":1,}", "x", "tru", "nul", "fals", "trx", "nulx",
  "[tru]", "[t]", "{\"a\":tx}", "-", "-a", "01", "1.", "1.e5", "1e", "1e+", "{\"a\":-}",
  "[-x]", "[1.5e+x]", "\"abc", "\"a\\x\"", "\"a\\u12\"", "\"a\\u12g4\"", "\"\t\"",
  "[\"a\u0000\"]", "\"", "\"\\", "\"\\u", "\"\\uD83D\\uDE0", "[] x", "{} {}", "1 2",
  "\ufeff[]", "\u00a01", "{\"a\":1}x", "{\"a\":1}}", "[1]]", "{,}", "[,1]", ":", "}",
  "{\"a\":\"b\",\"c\"}", "x1111111111111111111", "x11111111111111111111",
  "[1,1,1,1,1,1,1,1,1,x]", "[1,1,1,1,1,1,1,1,1,1,x]",
  "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
  "{\"version\":1,\"baseRefName\":\"main\",\"baseRef\":\"refs/remotes/origin/main\" broken}",
  "{\"version\":1,\"baseRefName\":\"main\",\"baseRef\":\"refs/remotes/origin/main\",\"changeRequestLookupTarget\":x}",
  "[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,x,1,2,3,4,5,6,7,8,9,10,11,12,13,14]",
  "{\"é\":1,x}", "{\"😀\":1,x}", "[\"😀\" x]", "😀", "[1,😀]", "[1,\u2028x]",
  "{\n\"a\":1,\nx}", "{\r\n\"a\":1,\r\nx}", "\r\r[1 2]", "\r\n\r\n[1 2]", "[\"\u2028\" 2]",
  "\n[\"😀\" 2]", "[\"a\",\n\"😀\" 2]", "{\"a\"\n:1,\n\"b\" 2}", "[\"\\ud83d\"x]",
  "{\"a\":\"\\uD83D\" x}", "1234567890123456789012345678901234567890x",
  "aaaaaaaaaaaaaaa😀😀😀😀😀😀😀😀😀😀x",
];
const rows = [];
for (const input of cases) {
  try {
    JSON.parse(input);
    rows.push([input, null]);
  } catch (error) {
    rows.push([input, error.message]);
  }
}
process.stdout.write(JSON.stringify(rows, null, 2) + "\n");
