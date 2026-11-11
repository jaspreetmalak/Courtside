const config = __CONFIG__;
window.__courtsideEvents?.stop();
let stopped = false, socket, retry;
const state = {
    stop() {
        stopped = true;
        clearTimeout(retry);
        socket?.close();
        if (window.__courtsideEvents === state) delete window.__courtsideEvents;
    },
};
window.__courtsideEvents = state;
function connect() {
    if (stopped) return;
    socket = new WebSocket(config.url);
    socket.onmessage = (event) => {
        if (stopped) return;
        try { dioxus.send(JSON.parse(event.data)); } catch {}
    };
    socket.onclose = () => { if (!stopped) retry = setTimeout(connect, 2000); };
    socket.onerror = () => socket.close();
}
connect();
