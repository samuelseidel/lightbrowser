t('eval', function () { return eval('1 + 1'); });
t('new Function', function () { return new Function('return 2')(); });
setTimeout("log('string timer ran')", 0);
var bytes = new Uint8Array([0,97,115,109,1,0,0,0,1,5,1,96,0,1,127,3,2,1,0,7,7,1,3,111,110,101,0,0,10,6,1,4,0,65,1,11]);
t('validate', function () { return WebAssembly.validate(bytes); });
t('new Module', function () { return new WebAssembly.Instance(new WebAssembly.Module(bytes)).exports.one(); });
done();
