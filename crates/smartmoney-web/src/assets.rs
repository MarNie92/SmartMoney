//! Static CSS and JS, served with cache headers.

pub const CSS: &str = r#"
:root {
  --bg: #e9eef2;
  --surface: #f7f9fb;
  --ink: #17202a;
  --muted: #566372;
  --line: #cdd6de;
  --track: #dde4ea;
  --alert: #b86a00;
  --alert-bg: #fff2d9;
  --c-fresh: #2f7d8c;
  --c-asym: #3b6fb6;
  --c-size: #5a5fc8;
  --c-conc: #7b55b8;
  --c-type: #9a4e9e;
  --c-cluster: #b8477a;
  --focus: #3b6fb6;
  color-scheme: light;
}
@media (prefers-color-scheme: dark) {
  :root {
    --bg: #0e1620;
    --surface: #15202b;
    --ink: #e3e9ef;
    --muted: #93a1af;
    --line: #26333f;
    --track: #223040;
    --alert: #f0a830;
    --alert-bg: #2b2213;
    --c-fresh: #4fb3c2;
    --c-asym: #6f9be0;
    --c-size: #8a8ff0;
    --c-conc: #a684e0;
    --c-type: #c77ccb;
    --c-cluster: #e2759f;
    --focus: #8ab4f8;
    color-scheme: dark;
  }
}
* { box-sizing: border-box; }
html { -webkit-text-size-adjust: 100%; }
body {
  margin: 0;
  background: var(--bg);
  color: var(--ink);
  font-family: "Archivo", system-ui, -apple-system, "Segoe UI", sans-serif;
  font-size: 16px;
  line-height: 1.45;
  font-variant-numeric: tabular-nums;
}
a { color: inherit; text-underline-offset: 3px; }
a:focus-visible, summary:focus-visible, button:focus-visible, input:focus-visible {
  outline: 2px solid var(--focus);
  outline-offset: 2px;
  border-radius: 4px;
}
.wrap { max-width: 760px; margin: 0 auto; padding: 0 16px; }

header.top {
  display: flex; align-items: baseline; justify-content: space-between;
  gap: 12px; padding: 18px 0 10px;
}
.brand {
  font-stretch: 75%; font-weight: 800; font-size: 26px; letter-spacing: -0.01em;
  text-decoration: none;
}
.top nav { display: flex; gap: 14px; font-size: 15px; color: var(--muted); }
.top nav a { text-decoration: none; }
.top nav a[aria-current="page"] { color: var(--ink); font-weight: 600; }

.status { margin: 4px 0 14px; color: var(--muted); font-size: 15px; max-width: 62ch; }
.status strong { color: var(--ink); font-weight: 600; }
.pulse {
  display: inline-block; width: 8px; height: 8px; border-radius: 50%;
  background: var(--c-fresh); margin-right: 6px; vertical-align: 1px;
}
.pulse.stale { background: var(--alert); }

.filters { display: flex; gap: 6px; flex-wrap: wrap; margin-bottom: 10px; }
.filters a {
  padding: 6px 12px; border: 1px solid var(--line); border-radius: 999px;
  text-decoration: none; font-size: 14px; color: var(--muted); background: var(--surface);
}
.filters a[aria-current="true"] { color: var(--ink); border-color: var(--ink); font-weight: 600; }

.legend {
  display: flex; flex-wrap: wrap; gap: 4px 12px; margin: 0 0 14px;
  font-size: 13px; color: var(--muted); padding: 0; list-style: none;
}
.legend li::before {
  content: ""; display: inline-block; width: 10px; height: 10px; border-radius: 2px;
  margin-right: 5px; vertical-align: -1px; background: var(--sw);
}

.feed { list-style: none; margin: 0; padding: 0; border-top: 1px solid var(--line); }
.row { border-bottom: 1px solid var(--line); }
.row.alert { background: var(--alert-bg); box-shadow: inset 4px 0 0 var(--alert); }
.row summary {
  list-style: none; cursor: pointer;
  display: grid; grid-template-columns: 52px minmax(0, 1fr); gap: 12px;
  padding: 14px 12px 14px 10px;
}
.row summary::-webkit-details-marker { display: none; }
.score {
  font-stretch: 62%; font-weight: 800; font-size: 40px; line-height: 0.95;
  text-align: right; letter-spacing: -0.02em; color: var(--muted);
}
.score.mid { color: var(--ink); }
.row.alert .score { color: var(--alert); }
.q { margin: 0 0 3px; font-weight: 600; line-height: 1.3; overflow-wrap: anywhere; }
.trade { margin: 0 0 8px; font-size: 15px; }
.trade .side { font-weight: 700; }
.trade .side.sell { color: var(--muted); }
.trade .outcome { color: var(--muted); }

.strip { display: flex; gap: 2px; height: 10px; margin: 0 0 8px; }
.seg { position: relative; background: var(--track); border-radius: 2px; overflow: hidden; }
.seg i { position: absolute; inset: 0 auto 0 0; background: var(--sw); }
.s-fresh { --sw: var(--c-fresh); }
.s-asym { --sw: var(--c-asym); }
.s-size { --sw: var(--c-size); }
.s-conc { --sw: var(--c-conc); }
.s-type { --sw: var(--c-type); }
.s-cluster { --sw: var(--c-cluster); }

.meta { margin: 0; font-size: 13px; color: var(--muted); display: flex; flex-wrap: wrap; gap: 2px 14px; }

.why { margin: 0; padding: 0 12px 6px 74px; display: grid; gap: 6px; }
.why div { display: grid; grid-template-columns: 9.5em 4.2em 1fr; gap: 8px; font-size: 14px; align-items: baseline; }
.why dt { font-weight: 600; }
.why dt::before {
  content: ""; display: inline-block; width: 8px; height: 8px; border-radius: 2px;
  margin-right: 6px; background: var(--sw);
}
.why .pts { text-align: right; }
.why dd { margin: 0; color: var(--muted); }
.links { display: flex; gap: 16px; flex-wrap: wrap; padding: 6px 12px 16px 74px; margin: 0; font-size: 14px; }

.empty { padding: 28px 12px; color: var(--muted); }

.facts { display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); gap: 12px 20px; margin: 8px 0 20px; }
.facts div { border-top: 1px solid var(--line); padding-top: 6px; }
.facts dt { font-size: 13px; color: var(--muted); }
.facts dd { margin: 0; font-size: 20px; font-weight: 600; font-stretch: 87%; }
h1 { font-stretch: 75%; font-weight: 800; font-size: 30px; margin: 8px 0 2px; letter-spacing: -0.01em; }
h2 { font-size: 18px; margin: 26px 0 8px; }
.addr { font-family: ui-monospace, "SFMono-Regular", Consolas, monospace; font-size: 13px; color: var(--muted); overflow-wrap: anywhere; margin: 0 0 6px; }
table.pos { width: 100%; border-collapse: collapse; font-size: 14px; }
table.pos th { text-align: left; font-weight: 600; color: var(--muted); padding: 6px 4px; border-bottom: 1px solid var(--line); }
table.pos td { padding: 8px 4px; border-bottom: 1px solid var(--line); vertical-align: top; }
table.pos .num { text-align: right; white-space: nowrap; }

.login { max-width: 360px; margin: 18vh auto 0; padding: 0 16px; }
.login input { width: 100%; font: inherit; padding: 10px 12px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface); color: var(--ink); }
.login button { margin-top: 10px; font: inherit; font-weight: 600; padding: 10px 16px; border: 0; border-radius: 8px; background: var(--ink); color: var(--bg); cursor: pointer; }

footer { color: var(--muted); font-size: 13px; padding: 24px 0 40px; }

@media (max-width: 520px) {
  .row summary { grid-template-columns: 44px minmax(0, 1fr); gap: 10px; }
  .score { font-size: 34px; }
  .why, .links { padding-left: 12px; }
  .why div { grid-template-columns: 1fr auto; }
  .why div { gap: 2px 8px; }
  .why dd:not(.pts) { grid-column: 1 / -1; padding-left: 14px; }
}
@media (prefers-reduced-motion: no-preference) {
  .row[data-new] { animation: arrive 1.2s ease-out; }
  @keyframes arrive { from { background: var(--alert-bg); } }
}
"#;

/// Refreshes the live region every 20 s unless a row is expanded or the tab
/// is hidden. New rows are marked so they flash once.
pub const JS: &str = r#"
(() => {
  const live = document.getElementById('live');
  if (!live || !live.dataset.src) return;
  const seen = new Set([...live.querySelectorAll('.row')].map(r => r.dataset.id));
  async function refresh() {
    if (document.hidden || live.querySelector('details[open]')) return;
    try {
      const res = await fetch(live.dataset.src, { credentials: 'same-origin' });
      if (!res.ok) return;
      live.innerHTML = await res.text();
      live.querySelectorAll('.row').forEach(r => {
        if (!seen.has(r.dataset.id)) { r.dataset.new = ''; seen.add(r.dataset.id); }
      });
    } catch (_) { /* offline: keep current view */ }
  }
  setInterval(refresh, 20000);
  document.addEventListener('visibilitychange', () => { if (!document.hidden) refresh(); });
})();
"#;
