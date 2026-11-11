const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
import { mkdir } from 'node:fs/promises';
import assert from 'node:assert/strict';
import {randomUUID} from 'node:crypto';

await mkdir('../target/web-qa', {recursive:true});
const browser=await chromium.launch({headless:true,executablePath:process.env.CHROME_EXECUTABLE_PATH});
const failures=[];
const user={id:'11111111-1111-4111-8111-111111111111',display_name:'Browser Tester',email:'browser@example.test'};
const chat={chat_id:'22222222-2222-4222-8222-222222222222',messages:[]};
async function mockAPI(page,live=false) {
  const at=new Date().toISOString();
  const call={id:"66666666-6666-4666-8666-666666666666",direction:"outbound",peer_number:"+14155550100",task_prompt:"Discuss my dental booking.",ai_backend:"open_ai_live",status:"in_progress",started_at:at,ended_at:null,summary:null,connected_seconds:0,created_at:at};
  const detail={call,transcript:[{id:randomUUID(),call_id:call.id,speaker:"callee",text:"What is your booking reference?",at}],mode:"assistant",control_message:"",chat_id:chat.chat_id,handoff:null,remembering:true,pending_question:{id:randomUUID(),chat_id:chat.chat_id,question:"What booking reference did I give you for Cedar Dental?",expires_at:new Date(Date.now()+(live==='expiring'?4000:20000)).toISOString()}};
  const historySettings={enabled:true};
  await page.addInitScript(({callId}) => {
    window.__fixtureRemembering=true;
    const sockets=new Set();
    class FixtureEvents {
      static OPEN=1;
      constructor() {this.readyState=1;sockets.add(this);setTimeout(()=>this.onopen?.({}),0);}
      close(){this.readyState=3;sockets.delete(this);}
    }
    window.WebSocket=FixtureEvents;
    window.__emitFixtureEvent=event=>{
      if(event.type==="call_recall_changed" && event.call_id===callId)window.__fixtureRemembering=event.remembering;
      for(const socket of sockets)socket.onmessage?.({data:JSON.stringify(event)});
    };
  },{callId:live?call.id:null});
  await page.route('**/api/**', async route => {
    const request=route.request(), path=new URL(request.url()).pathname;
    let response={},status=200;
    if(path==='/api/auth/login') {
      assert.deepEqual(request.postDataJSON(), {email:'browser@example.test',password:'test-password'});
      response={token:'test-browser-token',user};
    } else {
      assert.equal(request.headers().authorization,'Bearer test-browser-token');
      if(path==='/api/account/status') response={phone_verified:true,device_attested:false,app_attest_required:false,verified_phone_masked:'••• 1234',tier:'free',entitlement:'free_trial',trial_seconds_limit:900,trial_seconds_used:0,included_seconds_limit:0,included_seconds_used:0,period_end:null,dedicated_number:null,needs_number_claim:false};
      else if(path==='/api/account/call-preferences') response={voice:'marin',announce_courtside:true,custom_instructions:'',takeover_phone_number:''};
      else if(path==='/api/account/history-settings') {
        if(request.method()==='PUT') historySettings.enabled=request.postDataJSON().enabled;
        else assert.equal(request.method(),'GET');
        response=historySettings;
      }
      else if(path==='/api/calls') response={calls:live?[call]:[]};
      else if(live && path===`/api/calls/${call.id}`) response={...detail,remembering:await page.evaluate(()=>window.__fixtureRemembering)};
      else if(path==='/api/chats') response={chats:[]};
      else if(path==='/api/chat/new'||(path==='/api/chat'&&request.method()==='GET')) response=live?{...chat,call:detail}:chat;
      else if(path==='/api/chat') {
        const body=request.postDataJSON();
        if(body.call_answer) {
          assert.deepEqual(body.call_answer,{call_id:call.id,question_id:detail.pending_question.id});
          assert.equal(body.message,'October 18, 2010');
          const now=new Date().toISOString();
          response={user_message:{id:randomUUID(),role:'user',content:body.message,at:now},reply:{id:randomUUID(),role:'assistant',content:'Sent your answer to Courtside on the live call.',at:now}};
          detail.pending_question=null;detail.remembering=false;
          await page.evaluate(({callId,message})=>{
            window.__fixtureAnswerSubmitted=true;
            window.__emitFixtureEvent({type:'call_recall_changed',call_id:callId,remembering:false});
            window.__emitFixtureEvent({type:'call_question_changed',call_id:callId,question:null});
            window.__emitFixtureEvent({type:'chat_message_added',call_id:callId,message});
          },{callId:call.id,message:response.user_message});
          await route.fulfill({status:200,contentType:'application/json',body:JSON.stringify(response)});
          return;
        }
        assert.match(body.request_id,/^[0-9a-f-]{36}$/);
        const at=new Date().toISOString();
        if(body.message==='What booking reference did I give you for Cedar Dental?') {
          const activity={type:'chat_activity',chat_id:chat.chat_id,request_id:body.request_id,activity:'searching_chats'};
          await page.evaluate(event=>{window.__historyPermitRead=false;window.__historyPermitFinish=false;window.__emitFixtureEvent(event);},activity);
          await page.waitForFunction(()=>window.__historyPermitRead);
          await page.evaluate(event=>window.__emitFixtureEvent(event),{...activity,activity:'reading_chat'});
          await page.waitForFunction(()=>window.__historyPermitFinish);
          response={user_message:{id:randomUUID(),role:'user',content:body.message,at},reply:{id:randomUUID(),role:'assistant',content:'In our earlier Cedar Dental chat, you gave booking reference ZX-4827.',at}};
        } else {
          assert.equal(body.message,'Help me plan a reservation.');
          response={user_message:{id:'33333333-3333-4333-8333-333333333333',role:'user',content:body.message,at},reply:{id:'44444444-4444-4444-8444-444444444444',role:'assistant',content:'What restaurant and time should I plan for?',at}};
        }
      } else {status=404;response='Unknown fixture route';}
    }
    await route.fulfill({status,contentType:'application/json',headers:{'Access-Control-Allow-Origin':'*','Access-Control-Allow-Headers':'authorization,content-type,ngrok-skip-browser-warning'},body:JSON.stringify(response)});
  });
}
async function checkHistoryControls(page,name) {
  const history=page.getByRole('switch',{name:'Reference past chats'});
  await history.waitFor();
  assert.equal(await history.getAttribute('aria-checked'),'true');
  await history.click();
  await page.waitForFunction(()=>document.querySelector('[aria-label="Reference past chats"]').getAttribute('aria-checked')==='false');
  await history.click();
  await page.waitForFunction(()=>document.querySelector('[aria-label="Reference past chats"]').getAttribute('aria-checked')==='true');
  assert.equal(await page.locator('.memory-panel').count(),0);
  assert.equal(await page.getByRole('button',{name:'Add memory',exact:true}).count(),0);
  assert.equal(await page.getByText('MEMORY',{exact:true}).count(),0);
  await page.locator('.history-preference').scrollIntoViewIfNeeded();
  await page.screenshot({path:`../target/web-qa/history-preference-${name}.png`});
  assert.equal(await page.evaluate(()=>document.querySelector('.history-preference').getBoundingClientRect().right<=innerWidth),true);
}
async function checkHistoryActivity(page,name) {
  if(await page.locator('details.browser-tools[open]').count()) {
    await page.getByLabel('Call planning shortcuts').click();
  }
  await page.getByRole('textbox',{name:'Message Courtside'}).click();
  await page.getByRole('textbox',{name:'Message Courtside'}).fill('What booking reference did I give you for Cedar Dental?');
  await page.getByRole('textbox',{name:'Message Courtside'}).press('Enter');
  await page.getByText('Remembering…',{exact:true}).waitFor();
  // Activity from another request or another chat cannot replace this status.
  await page.evaluate(chatId=>{
    window.__emitFixtureEvent({type:'chat_activity',chat_id:chatId,request_id:'77777777-7777-4777-8777-777777777777',activity:null});
    window.__emitFixtureEvent({type:'chat_activity',chat_id:'88888888-8888-4888-8888-888888888888',request_id:'77777777-7777-4777-8777-777777777777',activity:null});
  },chat.chat_id);
  await page.waitForTimeout(150);
  assert.equal(await page.getByText('Remembering…',{exact:true}).isVisible(),true);
  assert.equal(await page.locator('.history-activity').getAttribute('data-activity'),'searching');
  await page.screenshot({path:`../target/web-qa/history-search-${name}.png`});
  await page.evaluate(()=>window.__historyPermitRead=true);
  await page.waitForFunction(()=>document.querySelector('.history-activity')?.getAttribute('data-activity')==='reading');
  await page.screenshot({path:`../target/web-qa/history-read-${name}.png`});
  await page.evaluate(()=>window.__historyPermitFinish=true);
  await page.getByText('In our earlier Cedar Dental chat, you gave booking reference ZX-4827.',{exact:true}).waitFor();
  assert.equal(await page.locator('.history-activity').count(),0);
}
async function checkLiveRemembering(name,width,height) {
  const page=await browser.newPage({viewport:{width,height}});
  page.on('pageerror',error=>failures.push(`live ${name}: ${error.message}`));
  await mockAPI(page,true);
  await page.goto('http://127.0.0.1:8080/app/?login=1');
  await page.getByPlaceholder('you@example.com').fill('browser@example.test');
  await page.getByPlaceholder('At least 8 characters').fill('test-password');
  await page.getByRole('button',{name:'Sign in',exact:true}).click();
  const status=page.locator('.call-remembering');
  await status.getByText('Remembering…',{exact:true}).waitFor();
  assert.equal(await page.getByText('What booking reference did I give you for Cedar Dental?',{exact:true}).count(),1);
  await page.evaluate(()=>window.__emitFixtureEvent({type:'call_recall_changed',call_id:'88888888-8888-4888-8888-888888888888',remembering:false}));
  assert.equal(await status.isVisible(),true);
  assert.equal(await page.locator('.live-call-card .call-question-prompt').count(),0);
  const dockQuestion=page.locator('.chat-dock > .call-question-prompt');
  assert.equal(await dockQuestion.isVisible(),true);
  assert.equal(await dockQuestion.evaluate(question=>{
    const rect=question.getBoundingClientRect();
    const composer=document.querySelector('.composer-row').getBoundingClientRect();
    return rect.top>=0 && rect.bottom<=composer.top && rect.right<=innerWidth;
  }),true,'question stays outside the transcript and above the composer');
  await page.screenshot({path:`../target/web-qa/call-remembering-${name}.png`});
  if(name==='mobile') {
    await page.getByRole('textbox',{name:'Answer the caller',exact:true}).focus();
    await page.evaluate(({height,width})=>window.courtsideLayout.keyboardChanged({height:height-342,fullHeight:height,width,duration:0,curve:0}),{height,width});
    assert.equal(await dockQuestion.evaluate(question=>{
      const rect=question.getBoundingClientRect();
      const composer=document.querySelector('.composer-row').getBoundingClientRect();
      const viewport=parseFloat(document.documentElement.style.getPropertyValue('--viewport-height'));
      return rect.top>=0 && rect.bottom<=composer.top && composer.bottom<=viewport;
    }),true,'answer panel and composer stay above the keyboard');
    await page.screenshot({path:'../target/web-qa/question-dock-keyboard-mobile.png'});
    await page.evaluate(({height,width})=>window.courtsideLayout.keyboardChanged({height,fullHeight:height,width,duration:0,curve:0}),{height,width});
  }
  await page.evaluate(()=>window.__emitFixtureEvent({type:'call_recall_changed',call_id:'66666666-6666-4666-8666-666666666666',remembering:false}));
  await status.waitFor({state:'detached'});
  await page.evaluate(()=>window.__emitFixtureEvent({type:'call_recall_changed',call_id:'88888888-8888-4888-8888-888888888888',remembering:true}));
  assert.equal(await status.count(),0);
  await page.getByRole('textbox',{name:'Answer the caller',exact:true}).fill('October 18, 2010');
  await page.getByRole('button',{name:'Send answer',exact:true}).click();
  await page.waitForFunction(()=>window.__fixtureAnswerSubmitted);
  await page.getByRole('textbox',{name:'Answer the caller',exact:true}).waitFor({state:'detached'});
  await page.getByText('October 18, 2010',{exact:true}).waitFor();
  await page.close();
}
async function checkInlineAnswerExpiry() {
  const page=await browser.newPage({viewport:{width:393,height:852}});
  page.on('pageerror',error=>failures.push(`expiry: ${error.message}`));
  await mockAPI(page,'expiring');
  await page.goto('http://127.0.0.1:8080/app/?login=1');
  await page.getByPlaceholder('you@example.com').fill('browser@example.test');
  await page.getByPlaceholder('At least 8 characters').fill('test-password');
  await page.getByRole('button',{name:'Sign in',exact:true}).click();
  const field=page.getByRole('textbox',{name:'Answer the caller',exact:true});
  await field.waitFor();
  await field.fill('Not sent');
  await field.waitFor({state:'detached',timeout:6000});
  assert.equal(await page.locator('.call-question-prompt').count(),0);
  assert.equal(await page.evaluate(()=>Boolean(window.__fixtureAnswerSubmitted)),false);
  await page.close();
}
try {
  for (const [name,width,height] of [['desktop',1440,1000],['mobile',393,852]]) {
    const page=await browser.newPage({viewport:{width,height},deviceScaleFactor:1});
    page.on('pageerror',error=>failures.push(`${name}: ${error.message}`));
    await page.goto('http://127.0.0.1:8080/');
    await page.getByRole('heading',{name:'Your calls. Handled.'}).waitFor();
    await page.getByRole('button',{name:'Appointments',exact:true}).click();
    await page.getByRole('heading',{name:'Dental office',exact:true}).waitFor();
    await page.waitForTimeout(400);
    assert.equal(await page.evaluate(()=>document.querySelector('.web-root').scrollWidth>innerWidth+1),false,'landing has horizontal overflow');
    await page.screenshot({path:`../target/web-qa/landing-${name}.png`,fullPage:true});
    await page.getByRole('link',{name:'Try the demo'}).click();
    await page.getByRole('button',{name:'Start the guided demo'}).click();
    await page.getByRole('heading',{name:'Ember restaurant'}).waitFor();
    await page.getByRole('button',{name:'Take over',exact:false}).click();
    await page.getByText('You’d now speak through your microphone',{exact:false}).waitFor();
    await page.getByRole('button',{name:'Return to AI',exact:false}).click();
    for (let i=0;i<5;i++) await page.getByRole('button',{name:i===4?'Show the outcome':'Continue the conversation',exact:false}).click();
    await page.getByRole('heading',{name:'Dinner plans, handled.'}).waitFor();
    assert.equal(await page.evaluate(()=>document.querySelector('.web-root').scrollWidth>innerWidth+1),false,'demo has horizontal overflow');
    await page.screenshot({path:`../target/web-qa/demo-${name}.png`,fullPage:true});
    await page.close();
  }
  const page=await browser.newPage({viewport:{width:1440,height:1000}});
  page.on('pageerror',error=>failures.push(`app: ${error.message}`));
  await mockAPI(page);
  await page.goto('http://127.0.0.1:8080/app/?login=1');
  await page.getByPlaceholder('you@example.com').fill('browser@example.test');
  await page.getByPlaceholder('At least 8 characters').fill('test-password');
  await page.getByRole('button',{name:'Sign in',exact:true}).click();
  await page.getByRole('heading',{name:'What should I handle?'}).waitFor();
  await page.waitForTimeout(600);
  await page.screenshot({path:'../target/web-qa/app-desktop.png',fullPage:true});
  await page.getByRole('textbox',{name:'Message Courtside'}).fill('Help me plan a reservation.');
  await page.getByRole('textbox',{name:'Message Courtside'}).press('Enter');
  await page.getByText('What restaurant and time should I plan for?',{exact:true}).waitFor();
  assert.equal(await page.getByRole('textbox',{name:'Message Courtside'}).inputValue(),'');
  await checkHistoryActivity(page,'desktop');
  await page.reload();
  await page.getByRole('heading',{name:'What should I handle?'}).waitFor();
  await page.getByRole('button',{name:'Calls',exact:true}).click();
  await page.getByRole('heading',{name:'Calls',exact:true}).waitFor();
  await page.getByRole('button',{name:'Open Account and Plan'}).click();
  await page.getByText('Browser Tester',{exact:true}).first().waitFor();
  await page.waitForTimeout(400);
  await page.screenshot({path:'../target/web-qa/account-desktop.png',fullPage:true});
  await checkHistoryControls(page,'desktop');
  await page.close();
  const mobile=await browser.newPage({viewport:{width:393,height:852}});
  mobile.on('pageerror',error=>failures.push(`app mobile: ${error.message}`));
  await mockAPI(mobile);
  await mobile.goto('http://127.0.0.1:8080/app/?login=1');
  await mobile.getByPlaceholder('you@example.com').fill('browser@example.test');
  await mobile.getByPlaceholder('At least 8 characters').fill('test-password');
  await mobile.getByRole('button',{name:'Sign in',exact:true}).click();
  await mobile.getByRole('heading',{name:'What should I handle?'}).waitFor();
  await mobile.waitForTimeout(600);
  await mobile.screenshot({path:'../target/web-qa/app-mobile.png'});
  await mobile.getByRole('button',{name:'Open navigation'}).click();
  await mobile.getByRole('button',{name:'Calls',exact:true}).click();
  await mobile.getByRole('heading',{name:'Calls',exact:true}).waitFor();
  await mobile.getByRole('button',{name:'Ask Courtside',exact:true}).click();
  await mobile.getByLabel('Call planning shortcuts').click();
  await mobile.getByRole('button',{name:'Schedule an appointment',exact:true}).click();
  assert.equal(await mobile.getByRole('textbox',{name:'Message Courtside'}).inputValue(),'Help me schedule an appointment');
  assert.equal(await mobile.evaluate(()=>document.querySelector('.composer').getBoundingClientRect().right<=innerWidth),true);
  await checkHistoryActivity(mobile,'mobile');
  await mobile.getByRole('button',{name:'Open navigation'}).click();
  await mobile.getByRole('button',{name:'Open Account and Plan'}).click();
  await checkHistoryControls(mobile,'mobile');
  await mobile.close();
  await checkLiveRemembering('desktop',1440,1000);
  await checkLiveRemembering('mobile',393,852);
  await checkInlineAnswerExpiry();
  assert.deepEqual(failures,[],'browser console errors');
  console.log('Passed: desktop/mobile landing, demo, sign-in, chat requests, Remembering activity with request and call isolation, session restore, navigation, history preference, scoped inline call answers, and countdown expiry without a server event.');
} finally {await browser.close();}
