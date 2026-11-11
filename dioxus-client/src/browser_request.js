const config = __CONFIG__;
try {
    const headers = { "ngrok-skip-browser-warning": "1" };
    if (config.token) headers.Authorization = `Bearer ${config.token}`;
    if (config.body !== null) headers["Content-Type"] = "application/json";
    const response = await fetch(config.url, {
        method: config.method, headers,
        body: config.body === null ? undefined : JSON.stringify(config.body),
        signal: AbortSignal.timeout(90000),
        credentials: "omit", cache: "no-store",
    });
    dioxus.send({ ok: response.ok, status: response.status, body: await response.text() });
} catch {
    dioxus.send({ ok: false, status: 0, body: "" });
}
