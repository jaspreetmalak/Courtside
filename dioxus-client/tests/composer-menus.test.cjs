const assert = require('node:assert/strict');
const { test } = require('node:test');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, '../src/composer-menus.js'), 'utf8');

function surface({ mounted = true } = {}) {
    const events = () => {
        const listeners = new Map();
        return {
            listeners,
            addEventListener(name, callback) {
                if (!listeners.has(name)) listeners.set(name, new Set());
                listeners.get(name).add(callback);
            },
            removeEventListener(name, callback) { listeners.get(name)?.delete(callback); },
            emit(name) { for (const callback of listeners.get(name) ?? []) callback(); },
        };
    };
    const tools = {
        disabled: false,
        rect: { x: 23, y: 768, width: 36, height: 36 },
        getBoundingClientRect() { return this.rect; },
    };
    const model = {
        disabled: false,
        rect: { x: 250, y: 768, width: 90, height: 36 },
        getBoundingClientRect() { return this.rect; },
    };
    const shell = {
        rect: { x: 0, y: 0, width: 393, height: 852 },
        getBoundingClientRect() { return this.rect; },
    };
    const nodes = new Map([
        ['courtside-webview', shell],
        ...(mounted ? [
        ['courtside-tools-button', tools], ['courtside-model-button', model],
        ] : []),
    ]);
    const viewport = { ...events(), offsetTop: 0, offsetLeft: 0 };
    const window = { ...events(), visualViewport: viewport };
    const composer = {};
    let covered = false;
    const document = {
        body: {},
        getElementById: (id) => nodes.get(id),
        querySelector: (selector) => selector === '.composer'
            ? nodes.has('courtside-tools-button') ? composer : null
            : covered ? {} : null,
    };
    const observers = [];
    class Observer {
        constructor(callback) { this.callback = callback; observers.push(this); }
        observe(target) { this.target = target; this.disconnected = false; }
        disconnect() { this.disconnected = true; }
        emit() { if (!this.disconnected) this.callback(); }
    }
    let nextFrame = 1;
    const frames = new Map();
    const sent = [];
    const context = vm.createContext({
        window, document,
        ResizeObserver: Observer, MutationObserver: Observer,
        requestAnimationFrame(callback) {
            const id = nextFrame++;
            frames.set(id, callback);
            return id;
        },
        cancelAnimationFrame: (id) => frames.delete(id),
        dioxus: { send: (value) => sent.push(JSON.parse(JSON.stringify(value))) },
    });
    const install = (owner) => vm.runInContext(`(async () => { ${source.replace('__OWNER__', owner)} })()`, context);
    const flush = () => {
        const queued = [...frames.values()];
        frames.clear();
        for (const callback of queued) callback();
    };
    const completion = install(1);
    return {
        window, viewport, document, tools, model, shell, nodes, composer, observers,
        sent, frames, completion, flush, install,
        cover: (value) => { covered = value; },
    };
}

test('native menus receive separate composer hit targets and suppress duplicate frames', async () => {
    const s = surface();
    s.flush();
    assert.deepEqual(s.sent[0].tools, s.tools.rect);
    assert.deepEqual(s.sent[0].model, s.model.rect);
    s.viewport.emit('resize');
    s.observers[0].emit();
    s.flush();
    assert.equal(s.sent.length, 1);
    s.window.courtsideComposerMenus.get(1)();
    await s.completion;
});

test('keyboard panning uses the shell origin for both WebKit client-rect conventions', async () => {
    const s = surface();
    s.flush();
    s.viewport.offsetTop = 210;
    s.viewport.offsetLeft = 4;
    for (const origin of [{ x: 4, y: 210 }, { x: 0, y: 0 }]) {
        s.shell.rect = { ...s.shell.rect, ...origin, height: 525 };
        s.tools.rect = { ...s.tools.rect, x: 23 + origin.x, y: 463 + origin.y };
        s.model.rect = { ...s.model.rect, x: 250 + origin.x, y: 463 + origin.y };
        s.viewport.emit('scroll');
        s.flush();
        assert.equal(s.sent.at(-1).tools.y, 463);
        assert.equal(s.sent.at(-1).model.y, 463);
        assert.equal(s.sent.at(-1).tools.x, 23);
        assert.equal(s.sent.at(-1).model.x, 250);
    }
    s.window.courtsideComposerMenus.get(1)();
    await s.completion;
});

test('the completed layout pass refreshes targets after the viewport event was measured', async () => {
    const s = surface();
    s.flush();
    s.viewport.emit('resize');
    s.flush();
    s.tools.rect.y = s.model.rect.y = 463;
    s.window.emit('courtside:layout');
    s.flush();
    assert.equal(s.sent.at(-1).tools.y, 463);
    assert.equal(s.sent.at(-1).model.y, 463);
    s.window.courtsideComposerMenus.get(1)();
    await s.completion;
});

test('a growing composer updates the menu targets even when button sizes stay the same', async () => {
    const s = surface();
    s.flush();
    assert.equal(s.observers[0].target, s.composer);
    s.tools.rect.y -= 48;
    s.model.rect.y -= 48;
    s.observers[0].emit();
    s.flush();
    assert.equal(s.sent.at(-1).tools.y, 720);
    assert.equal(s.sent.at(-1).model.y, 720);
    s.window.courtsideComposerMenus.get(1)();
    await s.completion;
});

test('drawer and startup overlays hide native hit targets and restore them on dismissal', async () => {
    const s = surface();
    s.flush();
    assert.equal(s.sent.at(-1).visible, true);
    s.cover(true);
    s.observers[1].emit();
    s.flush();
    assert.equal(s.sent.at(-1).visible, false);
    s.cover(false);
    s.observers[1].emit();
    s.flush();
    assert.equal(s.sent.at(-1).visible, true);
    s.window.courtsideComposerMenus.get(1)();
    await s.completion;
});

test('contact actions can be disabled independently of the model selector', async () => {
    const s = surface();
    s.flush();
    s.tools.disabled = true;
    s.observers[1].emit();
    s.flush();
    assert.equal(s.sent.at(-1).tools_enabled, false);
    assert.equal(s.sent.at(-1).model_enabled, true);
    s.window.courtsideComposerMenus.get(1)();
    await s.completion;
});

test('screen removal clears the native controls and all tracking listeners', async () => {
    const s = surface();
    s.flush();
    s.nodes.clear();
    s.observers[1].emit();
    s.flush();
    await s.completion;
    assert.equal(s.sent.at(-1), null);
    assert.equal(s.frames.size, 0);
    assert.equal(s.window.courtsideComposerMenus.size, 0);
    for (const observer of s.observers) assert.equal(observer.disconnected, true);
    for (const target of [s.window, s.viewport]) {
        for (const listeners of target.listeners.values()) assert.equal(listeners.size, 0);
    }
});

test('tracking waits for the first DOM mount rather than failing before buttons exist', async () => {
    const s = surface({ mounted: false });
    s.flush();
    assert.equal(s.sent.length, 0);
    assert.equal(s.frames.size, 1);
    s.nodes.set('courtside-tools-button', s.tools);
    s.nodes.set('courtside-model-button', s.model);
    s.flush();
    assert.equal(s.sent.length, 1);
    assert.equal(s.observers[0].target, s.composer);
    s.window.courtsideComposerMenus.get(1)();
    await s.completion;
});

test('disposing an old screen does not stop the replacement screen tracker', async () => {
    const s = surface();
    s.flush();
    const replacement = s.install(2);
    s.window.courtsideComposerMenus.get(1)();
    await s.completion;
    assert.equal(s.window.courtsideComposerMenus.has(2), true);
    s.flush();
    assert.notEqual(s.sent.at(-1), null);
    s.window.courtsideComposerMenus.get(2)();
    await replacement;
});
