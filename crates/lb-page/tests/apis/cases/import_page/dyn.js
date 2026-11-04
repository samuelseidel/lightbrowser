export async function load() { const m = await import('./sub/c.js'); return m.where; }
