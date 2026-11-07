document.addEventListener('securitypolicyviolation', function (e) {
  log('event ' + [e.constructor.name, e.type, e.bubbles, e.composed, e.cancelable, e.isTrusted, e.eventPhase, who(e.target), who(e.currentTarget)].join(' '));
});
window.addEventListener('securitypolicyviolation', function (e) { log('window saw ' + who(e.target)); });
document.onsecuritypolicyviolation = function () { log('document handler'); };
var i = document.createElement('img'); i.id = 'attached'; document.body.appendChild(i); i.src = 'pixel.png';
var j = document.createElement('img'); j.id = 'detached'; j.src = 'pixel.png?2';
t('ctor no init', function () { return new SecurityPolicyViolationEvent('x').violatedDirective; });
t('ctor', function () { var e = new SecurityPolicyViolationEvent('securitypolicyviolation', { violatedDirective: 'a', effectiveDirective: 'b', originalPolicy: 'c', disposition: 'report', statusCode: 7, blockedURI: 'u', lineNumber: 3 }); return [e.violatedDirective, e.effectiveDirective, e.originalPolicy, e.disposition, e.statusCode, e.blockedURI, e.lineNumber, e.columnNumber, JSON.stringify(e.sample), e.bubbles].join(' '); });
t('ctor init flags', function () { var e = new SecurityPolicyViolationEvent('x', { bubbles: false, cancelable: true, composed: false }); return [e.bubbles, e.cancelable, e.composed].join(' '); });
t('proto', function () { return Object.getOwnPropertyNames(SecurityPolicyViolationEvent.prototype).join(); });
done(500);
