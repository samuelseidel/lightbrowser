try { importScripts('dep.js'); postMessage('no throw'); } catch (e) { postMessage(e.name + ': ' + e.message); }
