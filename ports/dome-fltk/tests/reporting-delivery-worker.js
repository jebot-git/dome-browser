const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
async function until(fn) {
  const end = performance.now() + 7000;
  while (!await fn()) {
    if (performance.now() > end) throw new Error('Worker fixture timed out');
    await delay(10);
  }
}
onmessage = async event => {
  const cfg = event.data;
  const checks = [], reports = [], agents = [];
  const check = (passed, name) => {
    checks.push({name, passed: !!passed});
    if (!passed) throw new Error(name);
  };
  const observer = new ReportingObserver(batch => reports.push(...batch));
  observer.observe();
  const violate = () => {
    agents.push(navigator.userAgent);
    let denied = false;
    try { importScripts('data:text/javascript,void%200'); } catch (_) { denied = true; }
    check(denied, 'Worker CSP retains importScripts enforcement');
  };
  try {
    violate();
    await until(async () => (await (await fetch('/__counts?case=' + cfg.case)).json()).total >= 1);
    await delay(150);
    if (cfg.terminate) return;
    violate();
    await until(() => reports.length === 2);
    observer.disconnect();
    const buffered = [];
    const later = new ReportingObserver(batch => buffered.push(...batch), {buffered: true});
    later.observe();
    await until(() => buffered.length === 2);
    later.disconnect();
    check(JSON.stringify(buffered) === JSON.stringify(reports), 'Worker endpoint outcomes preserve observer history');
    check(reports.every(report => Object.keys(report).sort().join(',') === 'body,type,url'),
          'Worker observer snapshot excludes delivery bookkeeping');
    postMessage({checks, reports, agents});
  } catch (error) { postMessage({checks, reports, agents, error: String(error)}); }
};
