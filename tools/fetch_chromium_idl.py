"""Fetch every .idl file under Blink core/ and modules/ at a Chromium tag from gitiles."""
import base64, json, os, sys, urllib.request, concurrent.futures as cf
TAG = sys.argv[1]; OUT = sys.argv[2]
BASE = f"https://chromium.googlesource.com/chromium/src/+/refs/tags/{TAG}/third_party/blink/renderer/"
ROOTS = ["core", "modules"]
import time
def get(url):
    for i in range(8):
        try:
            with urllib.request.urlopen(url, timeout=60) as r: return r.read()
        except Exception as e:
            err = e; time.sleep(1.5 * (2 ** i))
    raise err
def listdir(path):
    raw = get(BASE + path + "/?format=JSON").decode()
    return json.loads(raw.split("\n", 1)[1])["entries"]
idl, dirs = [], list(ROOTS)
with cf.ThreadPoolExecutor(4) as ex:
    while dirs:
        batch, dirs = dirs, []
        for d, entries in zip(batch, ex.map(listdir, batch)):
            for e in entries:
                p = f"{d}/{e['name']}"
                if e["type"] == "tree": dirs.append(p)
                elif e["name"].endswith(".idl"): idl.append(p)
    print("idl files:", len(idl), flush=True)
    def fetch(p):
        data = base64.b64decode(get(BASE + p + "?format=TEXT"))
        dst = os.path.join(OUT, p); os.makedirs(os.path.dirname(dst), exist_ok=True)
        open(dst, "wb").write(data); return len(data)
    total = sum(ex.map(fetch, idl))
print("bytes:", total)
