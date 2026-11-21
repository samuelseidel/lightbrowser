self.onerror = function (msg, file, line, col, err) {
  postMessage('onerror args ' + msg + ' | ' + String(file).replace(/^.*\//, '') + ' ' + line + ':' + col + ' ' + (err instanceof Error) + ' ' + (err && err.message));
};
throw new Error('boom');
