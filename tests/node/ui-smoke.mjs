import puppeteer from 'puppeteer-core';
const shots = process.argv[2];
const base = 'http://localhost:9600';
const browser = await puppeteer.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true, args: ['--no-first-run'] });
const page = await browser.newPage();
await page.setViewport({ width: 1440, height: 900 });
const problems = [];
page.on('console', (m) => { if (m.type() === 'error') problems.push('console: ' + m.text()); });
page.on('pageerror', (e) => problems.push('pageerror: ' + e.message));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const step = async (name, fn) => { try { await fn(); console.log('ok  ', name); } catch (e) { problems.push(`${name}: ${e.message}`); console.log('FAIL', name, e.message); } };
const text = (sel) => page.$eval(sel, (e) => e.textContent);

await step('load data view', async () => {
  await page.goto(base + '/', { waitUntil: 'networkidle0' });
  await page.waitForSelector('.object-item');
  await page.waitForSelector('table.grid tbody tr');
  await page.screenshot({ path: `${shots}/1-accounts.png` });
});
await step('contacts table + open record', async () => {
  await page.goto(base + '/__localforce/ui/#/data/Contact', { waitUntil: 'networkidle0' });
  await page.waitForSelector('table.grid tbody tr.clickable');
  await sleep(300);
  await page.click('table.grid tbody tr.clickable');
  await page.waitForSelector('.drawer .form-row');
  await sleep(300);
  await page.screenshot({ path: `${shots}/2-contact-record.png` });
});
await step('edit + save a record', async () => {
  const row = await page.$$eval('.drawer .form-row', (rows) => rows.findIndex((r) => r.querySelector('.api')?.textContent === 'Title'));
  if (row < 0) throw new Error('no Title row');
  const input = (await page.$$('.drawer .form-row'))[row];
  await (await input.$('input')).type('VP Testing');
  await page.click('.drawer .btn.primary');
  await page.waitForSelector('.toast');
  const t = await text('.toast');
  if (!/saved/.test(t)) throw new Error('toast: ' + t);
});
await step('validation error on create', async () => {
  await page.goto(base + '/__localforce/ui/#/data/Contact/new', { waitUntil: 'networkidle0' });
  await page.waitForSelector('.drawer .form-row');
  await page.click('.drawer .btn.primary');
  await page.waitForSelector('.drawer .alert.err');
  const t = await text('.drawer .alert.err');
  if (!t.includes('REQUIRED_FIELD_MISSING')) throw new Error(t);
  await page.screenshot({ path: `${shots}/3-validation.png` });
});
await step('lookup picker', async () => {
  const rows = await page.$$('.drawer .form-row');
  for (const r of rows) {
    if ((await r.$eval('.api', (e) => e.textContent)) === 'AccountId') {
      await (await r.$('.lookup input')).type('Glo');
      await page.waitForSelector('.lookup-menu:not([hidden]) div');
      await page.screenshot({ path: `${shots}/4-lookup.png` });
      return;
    }
  }
  throw new Error('no AccountId row');
});
await step('new object + field', async () => {
  await page.goto(base + '/__localforce/ui/#/data/Account', { waitUntil: 'networkidle0' });
  await page.click('.sidebar-head .btn');
  await page.waitForSelector('.modal input');
  await page.type('.modal input', 'Widget');
  await page.screenshot({ path: `${shots}/5-new-object.png` });
  await page.click('.modal-foot .btn.primary');
  await page.waitForFunction(() => location.hash === '#/data/Widget__c/fields');
  await page.waitForSelector('.toolbar .btn.primary');
  await page.click('.toolbar .btn.primary');
  await page.waitForSelector('.modal select');
  await page.select('.modal select', 'picklist');
  const inputs = await page.$$('.modal input[type=text]');
  await inputs[0].type('Colour');
  await page.type('.modal textarea[placeholder="One value per line"]', 'Red\nGreen\nBlue');
  await page.screenshot({ path: `${shots}/6-new-field.png` });
  await page.click('.modal-foot .btn.primary');
  await page.waitForFunction(() => [...document.querySelectorAll('td.mono')].some((td) => td.textContent === 'Colour__c'));
  await page.screenshot({ path: `${shots}/7-fields.png` });
});
await step('create widget record', async () => {
  await page.goto(base + '/__localforce/ui/#/data/Widget__c/new', { waitUntil: 'networkidle0' });
  await page.waitForSelector('.drawer .form-row');
  await page.type('.drawer .form-row input[type=text]', 'Sprocket');
  await page.select('.drawer select', 'Green');
  await page.click('.drawer .btn.primary');
  await page.waitForFunction(() => /#\/data\/Widget__c\/a/.test(location.hash));
  await page.waitForSelector('table.grid tbody tr.clickable');
  await sleep(300);
  await page.screenshot({ path: `${shots}/8-widget.png` });
});
await step('soql console', async () => {
  await page.goto(base + '/__localforce/ui/#/query', { waitUntil: 'networkidle0' });
  await page.waitForSelector('.editor textarea');
  await page.click('.editor .btn.primary');
  await page.waitForSelector('.results table.grid');
  const sub = await page.$('.subrecords');
  if (sub) await sub.click();
  await sleep(200);
  await page.screenshot({ path: `${shots}/9-soql.png` });
  await page.$eval('.editor textarea', (t) => { t.value = 'SELECT Nmae FROM Account'; t.dispatchEvent(new Event('input')); });
  await page.click('.editor .btn.primary');
  await page.waitForSelector('.results .alert.err');
});
await step('api log', async () => {
  // Calls from an app (not this UI), including a failing one.
  await fetch(base + '/services/data/v62.0/sobjects/Account', { method: 'POST', headers: { authorization: 'Bearer x', 'content-type': 'application/json' }, body: '{"Name":"From an app"}' });
  await fetch(base + '/services/data/v62.0/query?q=' + encodeURIComponent('SELECT Nope FROM Account'), { headers: { authorization: 'Bearer x' } });
  await page.goto(base + '/__localforce/ui/#/log', { waitUntil: 'networkidle0' });
  await page.waitForSelector('#log-body table.grid tbody tr');
  const rows = await page.$$('#log-body tbody tr');
  for (const r of rows) if ((await r.$eval('td:nth-child(4)', (e) => e.textContent)).startsWith('4')) { await r.click(); break; }
  await page.waitForSelector('.log-detail pre');
  await sleep(200);
  await page.screenshot({ path: `${shots}/10-log.png` });
});
await step('connect dialog + dark theme', async () => {
  await page.goto(base + '/__localforce/ui/#/data/Opportunity', { waitUntil: 'networkidle0' });
  await page.click('#theme-btn');
  await page.click('#connect-btn');
  await page.waitForSelector('.modal .code-block');
  await page.screenshot({ path: `${shots}/11-connect-dark.png` });
});
await browser.close();
console.log(problems.length ? '\nPROBLEMS:\n' + problems.join('\n') : '\nNO PROBLEMS');
