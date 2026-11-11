// Desktop fallback. iPhone uses AVAudioSession + AVAudioEngine instead.
const { url, call_id } = __CONFIG__;
window.__courtsideListen?.stop();
await new Promise((resolve) => {
  const context = new (window.AudioContext || window.webkitAudioContext)();
  const socket = new WebSocket(url);
  socket.binaryType = 'arraybuffer';
  const tracks = new Map();
  let stopped = false;
  const emit = (type, message) => dioxus.send({ call_id, type, message });
  const stop = (type = 'ended', message = '') => {
    if (stopped) return;
    stopped = true;
    socket.close();
    context.close();
    if (window.__courtsideListen?.socket === socket) window.__courtsideListen = null;
    emit(type, message);
    resolve();
  };
  const clear = (track) => {
    for (const source of track.sources) source.stop();
    track.sources.clear();
    track.next = context.currentTime + 0.035;
  };
  const decodeMulaw = (byte) => {
    byte = (~byte) & 255;
    let sample = ((byte & 15) << 3) + 132;
    sample <<= (byte & 112) >> 4;
    return ((byte & 128) ? (132 - sample) : (sample - 132)) / 32768;
  };
  socket.onmessage = (event) => {
    if (stopped) return;
    if (typeof event.data === 'string') {
      const eventData = JSON.parse(event.data);
      if (eventData.type === 'connected') emit('connected', 'Listening live');
      else if (eventData.type === 'ended' || eventData.type === 'error') stop(eventData.type, eventData.message);
      return;
    }
    const bytes = new Uint8Array(event.data);
    const id = bytes[0];
    if (id < 1 || id > 3) return;
    if (!tracks.has(id)) tracks.set(id, { next: 0, sources: new Set() });
    const track = tracks.get(id);
    if (bytes.length === 1) { clear(track); return; }
    if (track.next - context.currentTime > (id === 2 ? 30 : 0.3)) clear(track);
    const buffer = context.createBuffer(1, bytes.length - 1, 8000);
    const channel = buffer.getChannelData(0);
    for (let index = 1; index < bytes.length; index++) channel[index - 1] = decodeMulaw(bytes[index]);
    const source = context.createBufferSource();
    source.buffer = buffer;
    source.connect(context.destination);
    source.onended = () => track.sources.delete(source);
    track.sources.add(source);
    const start = Math.max(track.next, context.currentTime + 0.035);
    source.start(start);
    track.next = start + buffer.duration;
  };
  socket.onerror = () => stop('error', 'Live audio disconnected. Tap Live listen to reconnect.');
  socket.onclose = () => stop('ended', 'Live audio ended. Tap Live listen to reconnect.');
  context.resume().catch(() => stop('error', 'Unable to start audio playback.'));
  window.__courtsideListen = { socket, context, call_id, stop: () => stop() };
  emit('connecting', 'Connecting live audio…');
});
