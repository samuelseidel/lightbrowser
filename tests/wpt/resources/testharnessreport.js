// LightBrowser's testharness report hook (replaces WPT's per-runner file).
// Reports every result as one console line the runner parses.
setup({ output: false, explicit_timeout: false });
add_completion_callback(function (tests, status) {
  var out = { status: status.status, message: status.message || null, tests: [] };
  for (var i = 0; i < tests.length; i++) {
    out.tests.push({ name: tests[i].name, status: tests[i].status, message: tests[i].message || null });
  }
  console.log("WPT_RESULT " + JSON.stringify(out));
});
