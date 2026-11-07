# Compare two chrome.mjs recordings (Chrome's chrome.json and one of
# LightBrowser through CDP=...), printing every value that differs.
# Usage: python3 compare.py chrome.json other.json
import json, sys
a, b = (json.load(open(p)) for p in sys.argv[1:3])
diffs = 0
def walk(x, y, path):
    global diffs
    if isinstance(x, dict) and isinstance(y, dict):
        for k in sorted(set(x) | set(y)):
            if k != 'version':
                walk(x.get(k), y.get(k), path + '.' + k)
    elif x != y:
        diffs += 1
        print('DIFF', path, 'a=', json.dumps(x, ensure_ascii=False)[:300], 'b=', json.dumps(y, ensure_ascii=False)[:300])
walk(a, b, '')
print(diffs, 'differences')
