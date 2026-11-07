// Time zone and locale as a realm sees them (window, frame or worker).
self.probe = function () {
  const dtf = Intl.DateTimeFormat().resolvedOptions();
  return {
    tz: dtf.timeZone,
    loc: dtf.locale,
    nloc: Intl.NumberFormat().resolvedOptions().locale,
    off: [new Date(2026, 0, 15).getTimezoneOffset(), new Date(2026, 6, 15).getTimezoneOffset()],
    str: new Date(0).toString(),
    ls: new Date(0).toLocaleString(),
    lds: new Date(0).toLocaleDateString(),
    lts: new Date(0).toLocaleTimeString(),
    num: (1234567.891).toLocaleString(),
    lang: navigator.language,
    langs: Array.from(navigator.languages),
  };
};
