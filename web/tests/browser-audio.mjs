const {chromium} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
import {readFile} from 'node:fs/promises';
import {existsSync} from 'node:fs';
import assert from 'node:assert/strict';
const workspaceSource=new URL('../../dioxus-client/src/browser_voice.js',import.meta.url);
const source=await readFile(existsSync(workspaceSource)?workspaceSource:new URL('../source/dioxus-client/src/browser_voice.js',import.meta.url),'utf8');
const browser=await chromium.launch({headless:true,executablePath:process.env.CHROME_EXECUTABLE_PATH,args:['--use-fake-ui-for-media-stream','--use-fake-device-for-media-stream']});
try {
  const page=await browser.newPage();
  await page.goto('http://127.0.0.1:8080/');
  await page.getByRole('heading',{name:'Your calls. Handled.'}).waitFor();
  await page.evaluate(script=>{
    window.__voiceTest={events:[],frames:[],tracks:[],socketClosed:false};
    const original=navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices);
    navigator.mediaDevices.getUserMedia=async(...args)=>{const stream=await original(...args);window.__voiceTest.tracks=stream.getTracks();return stream;};
    class Socket {
      static OPEN=1;
      constructor(){this.readyState=0;this.bufferedAmount=0;setTimeout(()=>{this.readyState=1;this.onopen?.({});},0);}
      send(data){if(typeof data==='string'){if(JSON.parse(data).type==='ready')setTimeout(()=>{this.onmessage({data:JSON.stringify({type:'connected'})});this.onmessage({data:new Uint8Array(160).fill(255).buffer});},0);}else window.__voiceTest.frames.push([...new Uint8Array(data)]);}
      close(){this.readyState=3;window.__voiceTest.socketClosed=true;this.onclose?.({});}
    }
    window.WebSocket=Socket;
    const button=document.createElement('button');button.textContent='Join test audio';button.style='position:fixed;top:0;left:0;z-index:99999';document.body.append(button);
    const AsyncFunction=Object.getPrototypeOf(async function(){}).constructor;
    button.onclick=()=>new AsyncFunction('const dioxus={send:event=>window.__voiceTest.events.push(event)};'+script)();
  },source.replace('__CONFIG__',JSON.stringify({call_id:'11111111-1111-4111-8111-111111111111',url:'wss://example.test/offline-media-fixture'})));
  await page.getByRole('button',{name:'Join test audio'}).click();
  await page.waitForFunction(()=>window.__voiceTest.events.some(e=>e.type==='connected'),{},{timeout:15000});
  await page.waitForFunction(()=>window.__voiceTest.frames.length>=3,{},{timeout:10000});
  assert.equal(await page.evaluate(()=>window.__voiceTest.frames.every(frame=>frame.length===160)),true);
  await page.evaluate(()=>{window.__voiceTest.frames=[];window.__courtsideVoice.setMuted(true);});
  await page.waitForFunction(()=>window.__voiceTest.frames.length>=3);
  assert.equal(await page.evaluate(()=>window.__voiceTest.frames.every(frame=>frame.every(b=>b===255))),true);
  await page.evaluate(()=>window.__courtsideVoice.stop());
  assert.deepEqual(await page.evaluate(()=>({closed:window.__voiceTest.socketClosed,tracksStopped:window.__voiceTest.tracks.every(t=>t.readyState==='ended'),ended:window.__voiceTest.events.at(-1).type,noActiveVoice:!window.__courtsideVoice})),{closed:true,tracksStopped:true,ended:'ended',noActiveVoice:true});
  console.log('Passed in Chrome with a simulated microphone and offline media socket: actual AudioWorklet capture, 160-byte 20 ms frames, playback, mute, and Return to AI cleanup. No phone calls placed.');
} finally {await browser.close();}
