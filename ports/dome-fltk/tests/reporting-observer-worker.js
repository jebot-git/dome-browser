const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
async function until(predicate) {
  const end = performance.now() + 6000;
  while (!predicate()) {
    if (performance.now() > end) throw new Error('Worker observer timed out');
    await delay(10);
  }
}
async function main() {
  const batches = [];
  let disconnected = false, deliveredAfterDisconnect = false;
  const observer = new ReportingObserver(reports => {
    batches.push(reports);
    deliveredAfterDisconnect = disconnected;
  });
  const driver = new ReportingObserver(() => {
    driver.disconnect();
    observer.disconnect();
    disconnected = true;
  });
  driver.observe();
  observer.observe();
  try { importScripts('data:text/javascript,void%200'); } catch (_) {}
  await until(() => batches.length > 0);
  if (!deliveredAfterDisconnect) throw new Error('Notification preceded disconnection');
  // Allow delivery to run before observing the independent history buffer.
  await delay(100);
  const buffered = [];
  const later = new ReportingObserver(reports => buffered.push(reports), {buffered: true});
  later.observe();
  await until(() => buffered.length > 0);
  later.disconnect();
  postMessage({queued:batches.flat().length, buffered:buffered.flat().length});
}
main().catch(error => postMessage({error:String(error)}));
