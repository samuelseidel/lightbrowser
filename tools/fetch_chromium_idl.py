"""Fetch every .idl file under Blink core/ and modules/ at a Chromium tag.

Directory listing comes from gitiles (chromium.googlesource.com); file bodies
come from the GitHub mirror (raw.githubusercontent.com), which is much faster
and byte-identical. Resumable: existing non-empty files are skipped and the
listing is cached in <out>/_list.txt.
"""
import base64, json, os, sys, time, urllib.request, concurrent.futures as cf

TAG = sys.argv[1]
OUT = sys.argv[2]
SUB = "third_party/blink/renderer/"
GITILES = f"https://chromium.googlesource.com/chromium/src/+/refs/tags/{TAG}/{SUB}"
RAW = f"https://raw.githubusercontent.com/chromium/chromium/{TAG}/{SUB}"
ROOTS = ["core", "modules"]


def get(url):
    err = None
    for i in range(8):
        try:
            with urllib.request.urlopen(url, timeout=60) as r:
                return r.read()
        except Exception as e:  # noqa: BLE001 - retry any transport error
            err = e
            time.sleep(min(30, 1.5 * (2 ** i)))
    raise err


def listdir(path):
    raw = get(GITILES + path + "/?format=JSON").decode()
    return json.loads(raw.split("\n", 1)[1])["entries"]


def listing():
    cache = os.path.join(OUT, "_list.txt")
    if os.path.exists(cache):
        return [l for l in open(cache).read().splitlines() if l]
    idl, dirs = [], list(ROOTS)
    with cf.ThreadPoolExecutor(4) as ex:
        while dirs:
            batch, dirs = dirs, []
            for d, entries in zip(batch, ex.map(listdir, batch)):
                for e in entries:
                    p = f"{d}/{e['name']}"
                    if e["type"] == "tree":
                        dirs.append(p)
                    elif e["name"].endswith(".idl"):
                        idl.append(p)
    idl.sort()
    os.makedirs(OUT, exist_ok=True)
    open(cache, "w").write("\n".join(idl))
    return idl


def fetch(p):
    dst = os.path.join(OUT, p)
    if os.path.exists(dst) and os.path.getsize(dst) > 0:
        return 0
    data = get(RAW + p)
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    open(dst, "wb").write(data)
    return len(data)


idl = listing()
print("idl files:", len(idl), flush=True)
with cf.ThreadPoolExecutor(16) as ex:
    total = sum(ex.map(fetch, idl))
have = sum(1 for p in idl if os.path.exists(os.path.join(OUT, p)))
print(f"fetched bytes: {total}; present: {have}/{len(idl)}")
