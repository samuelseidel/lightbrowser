/* deferred: runs before DOMContentLoaded */
(function () {
  'use strict';
  var links = document.querySelectorAll('nav.main a');
  for (var i = 0; i < links.length; i++) {
    links[i].addEventListener('click', function (e) { e.preventDefault(); });
    if (links[i].getAttribute('href') === location.pathname) links[i].classList.add('current');
  }
  var rows = document.querySelectorAll('table tbody tr');
  for (var j = 0; j < rows.length; j++) if (j % 2) rows[j].classList.add('odd');
  var imgs = document.images, n = 0;
  for (var k = 0; k < imgs.length; k++) if (imgs[k].getAttribute('alt')) n++;
  document.documentElement.setAttribute('data-bench-images', String(n));
  var y = document.getElementById('year');
  if (y) y.textContent = '2026';
  var h = document.body.getBoundingClientRect().height;
  document.documentElement.setAttribute('data-bench-laid-out', h >= 0 ? '1' : '0');
})();
