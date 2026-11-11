import test from 'node:test';
import assert from 'node:assert/strict';
import vm from 'node:vm';
import {readFile} from 'node:fs/promises';
import {existsSync} from 'node:fs';

const workspaceSource=new URL('../../dioxus-client/src/browser_voice.js',import.meta.url);
const script=await readFile(existsSync(workspaceSource)?workspaceSource:new URL('../source/dioxus-client/src/browser_voice.js',import.meta.url),'utf8');
const config={call_id:'11111111-1111-4111-8111-111111111111',url:'wss://example.test/voice'};
const flush=async()=>{for(let i=0;i<12;i++)await Promise.resolve();};
function setup({permission, resume}={}) {
  const events=[],sockets=[],contexts=[],captures=[],timers=new Map();
  const track={stopped:0,stop(){this.stopped++;}};
  const stream={getTracks:()=>[track]};
  const graph=()=>({disconnected:false,connect(){this.connected=true;},disconnect(){this.disconnected=true;}});
  class Audio {
    constructor(){this.state='suspended';this.currentTime=0;this.destination={};this.nodes=[];this.audioWorklet={addModule:async()=>{this.moduleReady=true;}};contexts.push(this);}
    async resume(){await resume;this.state='running';}
    async close(){this.state='closed';}
    createMediaStreamSource(){this.mic=graph();return this.mic;}
    createGain(){this.gain={...graph(),gain:{value:1}};return this.gain;}
    createBuffer(channels,length,rate){return {duration:length/rate,getChannelData:()=>new Float32Array(length)};}
    createBufferSource(){const node={...graph(),start(t){this.startTime=t;},stop(){this.stopped=true;}};this.nodes.push(node);return node;}
  }
  class Capture {
    constructor(context){assert.equal(context.moduleReady,true);this.port={onmessage:null};Object.assign(this,graph());captures.push(this);}
  }
  class Socket {
    static OPEN=1;
    constructor(){assert.equal(contexts.at(-1).state,'running');assert.ok(captures.at(-1).connected);this.readyState=0;this.bufferedAmount=0;this.sent=[];sockets.push(this);}
    send(data){this.sent.push(data);}
    close(){this.readyState=3;this.closed=true;this.onclose?.({});}
    open(){this.readyState=1;this.onopen({});}
    message(data){this.onmessage({data:typeof data==='object'&&!(data instanceof ArrayBuffer)?JSON.stringify(data):data});}
  }
  const window={AudioContext:Audio};
  const sandbox={window,navigator:{mediaDevices:{getUserMedia:()=>permission||Promise.resolve(stream)}},AudioWorkletNode:Capture,WebSocket:Socket,Uint8Array,ArrayBuffer,Float32Array,Blob,URL:{createObjectURL:()=> 'blob:module',revokeObjectURL(){}},dioxus:{send:e=>events.push(e)},setTimeout:fn=>{const id=timers.size+1;timers.set(id,fn);return id;},clearTimeout:id=>timers.delete(id)};
  const done=vm.runInNewContext(`(async()=>{${script.replace('__CONFIG__',JSON.stringify(config))}})()`,sandbox);
  return {done,window,track,stream,events,sockets,contexts,captures,timers};
}

test('takeover waits for running audio, sends ready, and waits for server acknowledgement before sending mic frames',async()=>{
  const h=setup();await h.done;
  assert.equal(h.sockets.length,1);
  const socket=h.sockets[0],capture=h.captures[0];socket.open();
  assert.deepEqual(JSON.parse(socket.sent[0]),{type:'ready'});
  const frame=new Uint8Array(160).fill(42).buffer;
  capture.port.onmessage({data:frame});assert.equal(socket.sent.length,1);
  socket.message({type:'connected'});assert.equal(h.events.at(-1).type,'connected');
  capture.port.onmessage({data:frame});assert.equal(socket.sent.at(-1),frame);
  h.window.__courtsideVoice.setMuted(true);capture.port.onmessage({data:frame});
  assert.equal(socket.sent.at(-1).length,160);assert.ok(socket.sent.at(-1).every(b=>b===255));
  socket.bufferedAmount=4001;const count=socket.sent.length;capture.port.onmessage({data:frame});assert.equal(socket.sent.length,count);
  socket.message(new Uint8Array(160).fill(255).buffer);assert.equal(h.contexts[0].nodes.length,1);
  h.window.__courtsideVoice.stop();
  assert.equal(h.track.stopped,1);assert.equal(h.contexts[0].state,'closed');assert.equal(socket.closed,true);
  assert.equal(h.contexts[0].nodes[0].stopped,true);assert.equal(capture.port.onmessage,null);
  assert.equal(h.window.__courtsideVoice,undefined);assert.equal(h.events.at(-1).type,'ended');assert.equal(h.timers.size,0);
});

test('denied microphone leaves the assistant connected without opening a voice socket',async()=>{
  const error=Object.assign(new Error('denied'),{name:'NotAllowedError'});
  const h=setup({permission:Promise.reject(error)});await h.done;
  assert.equal(h.sockets.length,0);assert.equal(h.events.at(-1).type,'error');
  assert.match(h.events.at(-1).message,/Allow microphone access/);assert.match(h.events.at(-1).message,/still has the call/);
  assert.equal(h.window.__courtsideVoice,undefined);
});

test('cancel while the permission prompt is open stops a late stream and never opens a media connection',async()=>{
  let allow;const h=setup({permission:new Promise(resolve=>allow=resolve)});
  await flush();h.window.__courtsideVoice.stop();allow(h.stream);await h.done;
  assert.equal(h.sockets.length,0);assert.equal(h.contexts.length,0);assert.equal(h.track.stopped,1);
  assert.equal(h.events.filter(e=>e.type==='ended').length,1);
});

test('audio activation timeout closes capture resources before the assistant is paused',async()=>{
  let activate;const h=setup({resume:new Promise(resolve=>activate=resolve)});await flush();
  assert.equal(h.timers.size,1);[...h.timers.values()][0]();activate();await h.done;
  assert.equal(h.sockets.length,0);assert.equal(h.track.stopped,1);assert.equal(h.events.at(-1).type,'error');
});

const workletSource=script.match(/const moduleSource = `([\s\S]*?)`;/)[1];
for(const rate of [8000,16000,44100,48000]) {
  test(`microphone worklet converts ${rate} Hz input into 8 kHz 20 ms mu-law frames without accumulating input`,()=>{
    let Processor;const frames=[];
    class Base {constructor(){this.port={postMessage:bytes=>frames.push(new Uint8Array(bytes))};}}
    vm.runInNewContext(workletSource,{AudioWorkletProcessor:Base,sampleRate:rate,Uint8Array,registerProcessor:(name,klass)=>{Processor=klass;}});
    const processor=new Processor();
    for(let offset=0;offset<rate;offset+=128)processor.process([[new Float32Array(Math.min(128,rate-offset))]]);
    assert.ok(frames.length>=49&&frames.length<=50);assert.ok(frames.every(f=>f.length===160&&f.every(b=>b===255)));
    assert.ok(processor.input.length<=2);assert.equal(processor.encode(0.5),143);assert.equal(processor.encode(-0.5),15);
  });
}
