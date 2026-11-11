// Full browser smoke check against the running Rust API; no phone calls.
const {chromium} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
import {randomUUID} from 'node:crypto';
import {spawnSync} from 'node:child_process';
import assert from 'node:assert/strict';

const email=`courtside-browser-qa-${randomUUID()}@example.test`;
const password=randomUUID();
const browser=await chromium.launch({headless:true,executablePath:process.env.CHROME_EXECUTABLE_PATH});
try {
  const page=await browser.newPage({viewport:{width:1440,height:1000}});
  const errors=[];page.on('pageerror',e=>errors.push(e.message));
  const eventConnection=page.waitForEvent('websocket',{predicate:socket=>new URL(socket.url()).pathname==='/api/ws',timeout:20000});
  await page.goto('http://127.0.0.1:8080/app/?signup=1');
  await page.getByPlaceholder('Your name').fill('Browser QA');
  await page.getByPlaceholder('you@example.com').fill(email);
  await page.getByPlaceholder('At least 8 characters').fill(password);
  await page.getByRole('button',{name:'Continue',exact:false}).click();
  await page.getByRole('button',{name:'Finish setup',exact:false}).click();
  await page.getByRole('heading',{name:'What should I handle?'}).waitFor();
  assert.ok(await page.evaluate(()=>JSON.parse(sessionStorage.getItem('courtside.session')).token.length>20));
  await eventConnection;
  const reply=page.waitForResponse(r=>new URL(r.url()).pathname==='/api/chat'&&r.request().method()==='POST',{timeout:90000});
  await page.getByRole('textbox',{name:'Message Courtside'}).fill('Hello. Briefly explain how you help plan calls. Do not make any phone calls.');
  await page.getByRole('textbox',{name:'Message Courtside'}).press('Enter');
  const response=await reply;assert.equal(response.status(),200,'real assistant chat request');
  const exchange=await response.json();assert.ok(exchange.reply.content.length>10);assert.equal(exchange.started_call??null,null,'smoke check must not start a phone call');
  await page.getByText(exchange.reply.content,{exact:true}).waitFor();
  await page.waitForTimeout(600);await page.screenshot({path:'../target/web-qa/app-real-backend.png'});
  await page.getByRole('button',{name:'Open Account and Plan'}).click();
  await page.getByRole('button',{name:'Verify my number',exact:true}).click();
  await page.getByRole('heading',{name:'Show your number'}).waitFor();
  await page.getByRole('button',{name:'Back to account',exact:true}).click();
  await page.getByRole('heading',{name:'Account',exact:true}).waitFor();
  await page.getByRole('button',{name:'Sign Out',exact:true}).click();
  assert.equal(await page.evaluate(()=>sessionStorage.getItem('courtside.session')),null);
  await page.goto('http://127.0.0.1:8080/app/?login=1');
  await page.getByPlaceholder('you@example.com').fill(email);
  await page.getByPlaceholder('At least 8 characters').fill(password);
  await page.getByRole('button',{name:'Sign in',exact:true}).click();
  await page.getByRole('heading',{name:'What should I handle?'}).waitFor();
  assert.deepEqual(errors,[]);
  console.log('Passed against the live Rust/ngrok backend: browser sign-up, unverified planning access, event WebSocket, real AI reply, caller-ID setup navigation, sign-out, and sign-in. No phone calls placed.');
} finally {
  await browser.close();
  // Delete only the unique, isolated smoke-test account and its test records.
  const cleanup=spawnSync('python3',['-c',`import sqlite3,sys
email=sys.argv[1]
assert email.startswith('courtside-browser-qa-') and email.endswith('@example.test')
con=sqlite3.connect(sys.argv[2])
row=con.execute('select id from users where email=?',(email,)).fetchone()
if row:
 for (table,) in con.execute("select name from sqlite_master where type='table'").fetchall():
  columns=[c[1] for c in con.execute('pragma table_info("'+table+'")')]
  if 'user_id' in columns: con.execute('delete from "'+table+'" where user_id=?',(row[0],))
 con.execute('delete from users where id=?',(row[0],));con.commit()
print('Cleaned isolated browser QA account.')`,email,process.env.COURTSIDE_QA_DATABASE || '../courtside.db'],{encoding:'utf8'});
  if(cleanup.status!==0)throw new Error('Unable to clean isolated QA account');
  console.log(cleanup.stdout.trim());
}
