// Shared by the CSP cases (loaded as an external script, so 'self' allows
// it). L is the log the test compares with Chrome's; every
// securitypolicyviolation that reaches the document is logged.
var L = [];
function log(s) { L.push(String(s)); }
function end() { log('end'); }
function later(f, ms) { setTimeout(f, ms === undefined ? 300 : ms); }
function done(ms) { later(end, ms); }
function err(e) {
  return e && e.name !== undefined
    ? Object.prototype.toString.call(e) + ' ' + e.name + ': ' + e.message
    : 'value ' + String(e);
}
function t(n, f) { try { log(n + ': ' + f()); } catch (e) { log(n + ' threw ' + err(e)); } }
async function a(n, f) { try { log(n + ': ' + await f()); } catch (e) { log(n + ' rejected ' + err(e)); } }
function who(x) {
  if (!x) return String(x);
  if (x === document) return '#document';
  if (x === window) return 'window';
  if (x.nodeType === 1) return x.tagName.toLowerCase() + (x.id ? '#' + x.id : '');
  return Object.prototype.toString.call(x);
}
var VFIELDS = ['documentURI', 'referrer', 'blockedURI', 'violatedDirective', 'effectiveDirective',
  'originalPolicy', 'disposition', 'sourceFile', 'sample', 'statusCode', 'lineNumber', 'columnNumber'];
function vdesc(e) {
  var o = {};
  VFIELDS.forEach(function (k) { o[k] = e[k]; });
  return who(e.target) + ' ' + JSON.stringify(o);
}
document.addEventListener('securitypolicyviolation', function (e) {
  log('violation ' + vdesc(e));
}, true);
