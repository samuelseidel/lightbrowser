t('eval', function () { return eval('var someLongVariableName = 1 + 2 + 3 + 4 + 5 + 6 + 7 + 8;'); });
t('new Function', function () { return new Function('a', 'b', 'return a + b'); });
