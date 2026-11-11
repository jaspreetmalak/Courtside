const config = __CONFIG__;
window.__courtsideVoice?.stop();
window.__courtsideListen?.stop();
let stopped = false, connected = false, muted = false;
let context, stream, socket, capture, source, silent, deadline;
let nextPlayback = 0;
const playback = new Set();
const emit = (type, message) => dioxus.send({ call_id: config.call_id, type, message });
const state = {
    call_id: config.call_id,
    setMuted(value) { muted = Boolean(value); },
    stop() { finish("ended", "Returning the call to Courtside…"); },
};
window.__courtsideVoice = state;
function finish(type, message) {
    if (stopped) return;
    stopped = true;
    connected = false;
    clearTimeout(deadline);
    socket?.close();
    if (capture) capture.port.onmessage = null;
    source?.disconnect(); capture?.disconnect(); silent?.disconnect();
    stream?.getTracks().forEach(track => track.stop());
    for (const node of playback) { try { node.stop(); } catch {} }
    playback.clear();
    context?.close().catch(() => {});
    if (window.__courtsideVoice === state) delete window.__courtsideVoice;
    emit(type, message);
}
function decode(byte) {
    const value = (~byte) & 255;
    const sample = (((value & 15) << 3) + 132) << ((value >> 4) & 7);
    return ((value & 128) ? 132 - sample : sample - 132) / 32768;
}
function play(bytes) {
    if (!connected || stopped || !bytes.length || bytes.length > 1600) return;
    if (nextPlayback - context.currentTime > 0.5) {
        for (const node of playback) { try { node.stop(); } catch {} }
        playback.clear(); nextPlayback = context.currentTime;
    }
    const buffer = context.createBuffer(1, bytes.length, 8000);
    const samples = buffer.getChannelData(0);
    for (let i = 0; i < bytes.length; i++) samples[i] = decode(bytes[i]);
    const node = context.createBufferSource();
    node.buffer = buffer; node.connect(context.destination);
    playback.add(node);
    node.onended = () => { playback.delete(node); node.disconnect(); };
    const start = Math.max(context.currentTime + 0.015, nextPlayback);
    node.start(start); nextPlayback = start + buffer.duration;
}
try {
    if (!navigator.mediaDevices?.getUserMedia) throw new Error("Microphone access requires a secure browser connection.");
    emit("connecting", "Allow microphone access to join the call.");
    stream = await navigator.mediaDevices.getUserMedia({audio:{echoCancellation:true,noiseSuppression:true,autoGainControl:true},video:false});
    if (stopped) { stream.getTracks().forEach(track => track.stop()); return; }
    const Audio = window.AudioContext || window.webkitAudioContext;
    if (!Audio) throw new Error("Call audio is unavailable in this browser.");
    context = new Audio();
    deadline = setTimeout(() => finish("error","Browser audio could not start. Try Take over again."),30000);
    await context.resume();
    if (stopped) return;
    if (context.state !== "running") throw new Error("Browser audio is paused. Try Take over again.");
    const moduleSource = `
        class CourtsideCapture extends AudioWorkletProcessor {
            constructor() { super(); this.input=[]; this.position=0; this.frame=new Uint8Array(160); this.used=0; }
            encode(value) {
                let sample=Math.max(-32768, Math.min(32767, Math.round(value*32768)));
                const sign=sample<0 ? 128 : 0;
                sample=Math.min(32635, Math.abs(sample))+132;
                let exponent=7;
                for (let mask=16384; exponent>0 && !(sample&mask); exponent--,mask>>=1) {}
                return (~(sign | (exponent<<4) | ((sample>>(exponent+3))&15)))&255;
            }
            process(inputs) {
                const samples=inputs[0]?.[0];
                if (!samples) return true;
                for (const value of samples) this.input.push(value);
                const ratio=sampleRate/8000;
                while (this.position+1<this.input.length) {
                    const i=Math.floor(this.position), part=this.position-i;
                    const value=this.input[i]*(1-part)+this.input[i+1]*part;
                    this.frame[this.used++]=this.encode(value);
                    if (this.used===160) { this.port.postMessage(this.frame.buffer,[this.frame.buffer]); this.frame=new Uint8Array(160); this.used=0; }
                    this.position+=ratio;
                }
                const consumed=Math.min(Math.floor(this.position), this.input.length-1);
                this.input.splice(0,consumed); this.position-=consumed;
                return true;
            }
        }
        registerProcessor("courtside-capture",CourtsideCapture);
    `;
    const moduleURL = URL.createObjectURL(new Blob([moduleSource], {type:"text/javascript"}));
    try { await context.audioWorklet.addModule(moduleURL); } finally { URL.revokeObjectURL(moduleURL); }
    if (stopped) return;
    source = context.createMediaStreamSource(stream);
    capture = new AudioWorkletNode(context,"courtside-capture");
    silent = context.createGain(); silent.gain.value=0;
    source.connect(capture); capture.connect(silent); silent.connect(context.destination);
    capture.port.onmessage = event => {
        if (!connected || stopped || socket?.readyState !== WebSocket.OPEN || socket.bufferedAmount > 4000) return;
        socket.send(muted ? new Uint8Array(160).fill(255) : event.data);
    };
    // Only open/ready the media socket once microphone capture and playback
    // are running. Until then, the existing assistant keeps the phone call.
    socket = new WebSocket(config.url); socket.binaryType="arraybuffer";
    clearTimeout(deadline);
    deadline = setTimeout(() => finish("error","Unable to join audio. Courtside still has the call."),30000);
    socket.onopen = () => { if (!stopped) socket.send(JSON.stringify({type:"ready"})); };
    socket.onmessage = event => {
        if (stopped) return;
        if (event.data instanceof ArrayBuffer) { play(new Uint8Array(event.data)); return; }
        try {
            const message=JSON.parse(event.data);
            if (message.type==="connected") { connected=true;clearTimeout(deadline);emit("connected","You’re on the call. Use Return to AI when you’re done."); }
            if (message.type==="error") finish("error",message.message || "Unable to join the call.");
            if (message.type==="ended") finish("ended",message.message || "Returning the call to Courtside…");
        } catch {}
    };
    socket.onerror = () => finish("error","Voice connection lost. Courtside will resume the call.");
    socket.onclose = () => finish("ended","Voice connection ended. Returning the call to Courtside…");
} catch (error) {
    if (stopped) return;
    const denied = error?.name === "NotAllowedError";
    finish("error", denied ? "Allow microphone access in your browser to join. Courtside still has the call." : (error?.message || "Couldn’t start browser call audio."));
}
