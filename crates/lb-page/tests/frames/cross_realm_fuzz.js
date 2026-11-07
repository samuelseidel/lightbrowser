// Calls every Node, Element, Document ... prototype member of one realm on
// nodes of another document (both directions) with a pool of arguments,
// and moves nodes back and forth. Returns the number of calls and any
// "internal error in host function" (a caught Rust panic).
(() => {
  let fe = document.getElementById('f1');
  let f = fe.contentWindow;
  // Fuzzed calls may remove the frame: make a new one then.
  const frame = () => {
    let ok = false;
    try { ok = fe.isConnected && !!f.document.documentElement; } catch (e) {}
    if (!ok) {
      if (!document.body) (document.documentElement || document.appendChild(document.createElement('html'))).appendChild(document.createElement('body'));
      fe = document.createElement('iframe');
      document.body.appendChild(fe);
      f = fe.contentWindow;
    }
    return f;
  };
  const skip = new Set(['constructor', 'write', 'writeln', 'open', 'close', 'location', 'click', 'submit', 'requestSubmit', 'requestFullscreen', 'webkitRequestFullscreen', 'webkitRequestFullScreen', 'requestPointerLock', 'showPicker', 'showModal', 'show', 'showPopover', 'togglePopover']);
  const ifaces = ['EventTarget', 'Node', 'Element', 'HTMLElement', 'Document', 'CharacterData', 'Text', 'DocumentFragment', 'HTMLInputElement', 'HTMLSelectElement', 'HTMLOptionElement', 'HTMLFormElement', 'HTMLAnchorElement', 'HTMLImageElement', 'HTMLCanvasElement', 'HTMLTableElement', 'HTMLTextAreaElement', 'HTMLButtonElement', 'HTMLMediaElement', 'HTMLTemplateElement', 'SVGElement'];
  const internal = []; let calls = 0;
  const fresh = (W) => {
    const D = W.document;
    try {
      if (!D.documentElement) D.appendChild(D.createElement('html'));
      if (!D.body) D.documentElement.appendChild(D.createElement('body'));
    } catch (e) {}
    const div = D.createElement('div');
    div.innerHTML = '<p id=p1>x<b>y</b></p><input value=v><select><option>o</option></select><a href="#h">a</a><textarea>t</textarea><button>b</button><template><i>t</i></template><svg><circle/></svg><video></video>';
    try { D.body.appendChild(div); } catch (e) {}
    const q = (s) => div.querySelector(s);
    return [div, q('p'), q('p').firstChild, q('input'), q('select'), q('option'), q('a'), q('textarea'), q('button'), q('template'), q('svg'), q('video'),
      D, D.body, D.documentElement, D.createTextNode('t'), D.createComment('c'), D.createDocumentFragment(), D.createElement('canvas'), D.createElement('table'), D.createElement('img'), D.createElement('form')].filter(Boolean);
  };
  const note = (where, e) => {
    const m = String(e && e.message);
    if (m.includes('internal error')) internal.push(where + ': ' + m);
  };
  for (const dir of ['main', 'frame']) {
    for (const name of ifaces) {
      const fw = frame();
      const [F, W] = dir === 'main' ? [window, fw] : [fw, window];
      const I = F[name];
      if (!I) continue;
      for (const k of Object.getOwnPropertyNames(I.prototype)) {
        if (skip.has(k)) continue;
        const d = Object.getOwnPropertyDescriptor(I.prototype, k);
        const recvs = fresh(W);
        const pool = [W.document.body, W.document, document.createElement('span'), frame().document.createElement('span'), recvs[1], recvs[2]];
        for (const r of recvs) {
          const at = (F === window ? 'main' : 'frame') + ' ' + name + '.' + k + ' on ' + (r.nodeName || r);
          const go = (fn) => { calls++; try { const v = fn(); if (v && typeof v.then === 'function') v.then(null, () => {}); } catch (e) { note(at, e); } };
          if (d.get) go(() => d.get.call(r));
          if (d.set) { go(() => d.set.call(r, 'x')); go(() => d.set.call(r, pool[0])); }
          if (typeof d.value === 'function') {
            go(() => d.value.call(r));
            for (const a of pool) go(() => d.value.call(r, a));
            go(() => d.value.call(r, pool[2], pool[3]));
            go(() => d.value.call(r, pool[4], pool[0]));
            go(() => d.value.call(r, 'div', 'x'));
            go(() => d.value.call(r, 1, 2));
          }
        }
      }
    }
  }
  // Moves back and forth.
  for (let i = 0; i < 20; i++) {
    const f = frame();
    const a = f.document.createElement('div'); a.innerHTML = '<b>1</b><i>2</i>';
    document.body.appendChild(a); f.document.body.appendChild(a); document.adoptNode(a); f.document.adoptNode(a.firstChild);
    try { a.replaceWith(f.document.createElement('p')); } catch (e) { note('move', e); }
  }
  return JSON.stringify({ calls, internal: internal.slice(0, 50), n: internal.length, alive: frame().document.nodeName });
})()
