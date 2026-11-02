// Site behaviour for the shop: a client-side cart. No globals.
(() => {
  'use strict';
  const items = [];
  const list = document.getElementById('cart-items');
  if (!list) return;
  const total = document.getElementById('cart-total');
  const ids = document.getElementById('cart-ids');
  function render() {
    list.textContent = '';
    if (!items.length) {
      const li = document.createElement('li');
      li.className = 'empty';
      li.textContent = 'Your cart is empty.';
      list.appendChild(li);
    }
    let sum = 0;
    for (const it of items) {
      const li = document.createElement('li');
      li.textContent = `${it.name}  ${it.price}`;
      list.appendChild(li);
      sum += parseFloat(it.price.replace('$', ''));
    }
    total.textContent = `$${sum.toFixed(2)}`;
    ids.value = items.map((i) => i.id).join(',');
  }
  document.addEventListener('click', (e) => {
    const b = e.target.closest && e.target.closest('button.add');
    if (!b) return;
    const card = b.closest('.card');
    items.push({
      id: b.dataset.id,
      name: card.querySelector('h3').textContent,
      price: card.querySelector('.price').textContent,
    });
    b.textContent = 'Added';
    render();
  });
})();
