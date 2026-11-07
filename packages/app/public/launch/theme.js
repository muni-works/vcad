// Pilot Light theme toggle — external so it runs under vcad.io's CSP (no inline scripts).
(() => {
  const r = document.documentElement;
  try { const t = localStorage.getItem('theme'); if (t) r.dataset.theme = t; } catch (e) {}
  document.addEventListener('DOMContentLoaded', () => {
    const b = document.getElementById('theme-toggle');
    if (!b) return;
    b.addEventListener('click', () => {
      const dark = r.dataset.theme ? r.dataset.theme === 'dark' : matchMedia('(prefers-color-scheme: dark)').matches;
      r.dataset.theme = dark ? 'light' : 'dark';
      try { localStorage.setItem('theme', r.dataset.theme); } catch (e) {}
    });
  });
})();
