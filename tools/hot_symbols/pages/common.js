(function () {
  const cards = document.getElementById('cards');
  const items = Array.from(document.querySelectorAll('#rows tr')).slice(0, 6);
  const by = new Map();
  for (const tr of items) {
    const cells = tr.querySelectorAll('td');
    by.set(cells[0].textContent, Number(cells[1].textContent));
  }
  for (const [hour, temp] of by) {
    const div = document.createElement('div');
    div.className = 'card';
    div.dataset.hour = hour;
    div.textContent = hour + ' ' + temp.toFixed(1) + ' C';
    cards.appendChild(div);
  }
  const first = cards.firstElementChild;
  const r = first ? first.getBoundingClientRect() : { width: 0 };
  const cs = getComputedStyle(cards);
  document.body.dataset.layout = [Math.round(r.width), cs.display, document.images.length, document.body.offsetHeight > 0].join(',');
  const form = document.getElementById('f');
  form.addEventListener('submit', (e) => { e.preventDefault(); form.dataset.sent = new FormData(form).get('name'); });
  form.querySelector('button').click();
  let n = 0;
  const tick = () => { if (++n < 3) setTimeout(tick, 0); else document.body.classList.add('ready'); };
  setTimeout(tick, 0);
  requestAnimationFrame(() => { document.body.dataset.frame = '1'; });
  try { localStorage.setItem('k', 'v'); sessionStorage.getItem('k'); } catch (e) { /* no storage */ }
  const xhr = new XMLHttpRequest();
  xhr.open('GET', 'common.css');
  xhr.onload = () => { document.body.dataset.css = String(xhr.responseText.length > 0); };
  xhr.send();
  fetch('common.css').then((res) => res.text()).then((t) => { document.body.dataset.fetched = String(t.split('\n').length); });
})();
