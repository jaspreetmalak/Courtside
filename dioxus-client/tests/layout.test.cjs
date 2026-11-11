const assert = require('node:assert/strict');
const { test } = require('node:test');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, '../src/layout.js'), 'utf8');

function surface({ visualViewport = true } = {}) {
    const events = () => {
        const listeners = new Map();
        return {
            listeners,
            addEventListener(name, listener) {
                if (!listeners.has(name)) listeners.set(name, new Set());
                listeners.get(name).add(listener);
            },
            removeEventListener(name, listener) {
                listeners.get(name)?.delete(listener);
            },
            emit(name, event = {}) {
                for (const listener of listeners.get(name) ?? []) listener(event);
            },
            dispatchEvent(event) { this.emit(event.type, event); },
        };
    };
    const properties = new Map();
    const classes = new Set();
    const root = {
        style: { setProperty: (name, value) => properties.set(name, value) },
        classList: {
            toggle: (name, enabled) => enabled ? classes.add(name) : classes.delete(name),
        },
    };
    const heightWrites = [];
    const composerStyle = {};
    let fieldHeight = '';
    Object.defineProperty(composerStyle, 'height', {
        get: () => fieldHeight,
        set(value) { fieldHeight = value; heightWrites.push(value); },
    });
    const composer = {
        id: 'courtside-composer-input',
        style: composerStyle,
        value: '',
        scrollHeight: 24,
        minHeight: '24px',
        maxHeight: '144px',
    };
    const chat = {
        id: 'courtside-chat-content',
        scrollHeight: 1400,
        clientHeight: 400,
        scrollTop: 0,
        scrolls: [],
        scrollTo(options) { this.scrolls.push(options); },
    };
    const viewport = {
        ...events(), height: 852, width: 393, offsetTop: 0, offsetLeft: 0, scale: 1,
    };
    const window = {
        ...events(), innerHeight: 852, innerWidth: 393,
        visualViewport: visualViewport ? viewport : undefined,
    };
    const document = {
        ...events(), documentElement: root, activeElement: null,
        getElementById: (id) => id === composer.id ? composer : id === chat.id ? chat : null,
        querySelectorAll: () => [],
    };
    let nextFrame = 1;
    const frames = new Map();
    let nextTimer = 1;
    const timers = new Map();
    const context = vm.createContext({
        window, document, Event,
        requestAnimationFrame(callback) {
            const id = nextFrame++;
            frames.set(id, callback);
            return id;
        },
        cancelAnimationFrame: (id) => frames.delete(id),
        setTimeout(callback) {
            const id = nextTimer++;
            timers.set(id, callback);
            return id;
        },
        clearTimeout: (id) => timers.delete(id),
        getComputedStyle: (element) => element,
    });
    const install = () => vm.runInContext(source, context);
    const flush = () => {
        const queued = [...frames.values()];
        frames.clear();
        for (const callback of queued) callback();
    };
    install();
    const finishTimers = () => {
        const callbacks = [...timers.values()];
        timers.clear();
        for (const callback of callbacks) callback();
    };
    return { window, document, viewport, composer, chat, properties, classes, frames, timers, heightWrites, flush, install, finishTimers };
}

test('keyboard resize cycles shrink and restore the visible app viewport', () => {
    const s = surface();
    assert.equal(s.properties.get('--viewport-height'), '852px');
    assert.equal(s.classes.has('keyboard-open'), false);
    for (const height of [525, 510, 525, 852, 525, 852]) {
        s.viewport.height = height;
        s.viewport.emit('resize');
        s.flush();
        assert.equal(s.properties.get('--viewport-height'), `${height}px`);
        assert.equal(s.classes.has('keyboard-open'), height < 732);
    }
});

test('WebKit shrinking both window and visual viewport still activates the keyboard layout', () => {
    const s = surface();
    for (const height of [525, 510, 525, 852, 525, 852]) {
        s.window.innerHeight = s.viewport.height = height;
        s.window.emit('resize');
        s.viewport.emit('resize');
        s.flush();
        assert.equal(s.properties.get('--viewport-height'), `${height}px`);
        assert.equal(s.classes.has('keyboard-open'), height < 732);
    }
});

test('rotation replaces the retained portrait height before tracking a landscape keyboard', () => {
    const s = surface();
    s.window.innerWidth = s.viewport.width = 852;
    s.window.innerHeight = s.viewport.height = 393;
    s.window.emit('orientationchange');
    s.flush();
    assert.equal(s.classes.has('keyboard-open'), false);
    s.window.innerHeight = s.viewport.height = 220;
    s.viewport.emit('resize');
    s.flush();
    assert.equal(s.classes.has('keyboard-open'), true);
    s.window.innerHeight = s.viewport.height = 393;
    s.viewport.emit('resize');
    s.flush();
    assert.equal(s.classes.has('keyboard-open'), false);
});

test('resize keeps recent messages visible and preserves the position when reading older messages', () => {
    const s = surface();
    s.chat.scrollTop = 1000;
    s.viewport.height = 525;
    s.viewport.emit('resize');
    s.flush();
    assert.equal(s.chat.scrollTop, 1400);
    s.chat.scrollTop = 200;
    s.viewport.height = 852;
    s.viewport.emit('resize');
    s.flush();
    assert.equal(s.chat.scrollTop, 200);
});

test('new messages scroll only their conversation container', () => {
    const s = surface();
    s.window.courtsideLayout.scrollToEnd(s.chat.id, 'smooth');
    assert.equal(s.chat.scrolls.length, 0);
    s.flush();
    assert.equal(s.chat.scrolls.length, 1);
    assert.equal(s.chat.scrolls[0].top, 1400);
    assert.equal(s.chat.scrolls[0].behavior, 'smooth');
    s.window.courtsideLayout.scrollToEnd('unmounted-transcript');
    assert.doesNotThrow(() => s.flush());
});

test('iOS focus panning is tracked and cleared after keyboard dismissal', () => {
    const s = surface();
    s.viewport.height = 525;
    s.viewport.offsetTop = 210;
    s.viewport.offsetLeft = 4;
    s.viewport.emit('scroll');
    s.flush();
    assert.equal(s.properties.get('--viewport-top'), '210px');
    assert.equal(s.properties.get('--viewport-left'), '4px');
    s.viewport.height = 852;
    s.viewport.offsetTop = s.viewport.offsetLeft = 0;
    s.viewport.emit('resize');
    s.flush();
    assert.equal(s.properties.get('--viewport-top'), '0px');
    assert.equal(s.properties.get('--viewport-left'), '0px');
    assert.equal(s.classes.has('keyboard-open'), false);
});

test('rotation updates both dimensions and hardware keyboard focus preserves bottom spacing', () => {
    const s = surface();
    s.document.activeElement = s.composer;
    s.window.emit('resize');
    s.flush();
    assert.equal(s.classes.has('keyboard-open'), false);
    s.window.innerHeight = s.viewport.height = 393;
    s.window.innerWidth = s.viewport.width = 852;
    s.window.emit('orientationchange');
    s.viewport.emit('resize');
    assert.equal(s.frames.size, 1);
    s.flush();
    assert.equal(s.properties.get('--viewport-height'), '393px');
    assert.equal(s.properties.get('--viewport-width'), '852px');
    assert.equal(s.classes.has('keyboard-open'), false);
});

test('multiline input grows, caps at the available height, and shrinks when cleared', () => {
    const s = surface();
    s.composer.value = 'First line\nSecond line\nThird line';
    s.composer.scrollHeight = 72;
    s.document.emit('input', { target: s.composer });
    s.flush();
    assert.equal(s.composer.style.height, '72px');
    assert.equal(s.composer.style.overflowY, 'hidden');
    s.composer.value = 'Many lines\n'.repeat(20);
    s.composer.scrollHeight = 480;
    s.composer.maxHeight = '131.25px';
    s.document.emit('input', { target: s.composer });
    s.flush();
    assert.equal(s.composer.style.height, '131.25px');
    assert.equal(s.composer.style.overflowY, 'auto');
    s.composer.value = '';
    s.composer.scrollHeight = 24;
    s.window.courtsideLayout.schedule();
    s.flush();
    assert.equal(s.composer.style.height, '24px');
    assert.equal(s.composer.style.overflowY, 'hidden');
});

test('native menu tracking is notified after viewport and composer layout have settled', () => {
    const s = surface();
    const observations = [];
    s.window.addEventListener('courtside:layout', () => observations.push({
        viewport: s.properties.get('--viewport-height'),
        composer: s.composer.style.height,
    }));
    s.viewport.height = 525;
    s.composer.scrollHeight = 72;
    s.composer.value = 'First line\nSecond line\nThird line';
    s.viewport.emit('resize');
    s.flush();
    assert.deepEqual(observations, [{ viewport: '525px', composer: '72px' }]);
});

test('viewport animation frames do not collapse the unchanged focused textarea', () => {
    const s = surface();
    s.document.activeElement = s.composer;
    s.heightWrites.length = 0;
    s.window.courtsideLayout.schedule();
    s.flush();
    for (const height of [800, 680, 580, 525, 510, 525]) {
        s.viewport.height = s.window.innerHeight = height;
        s.viewport.emit('resize');
        s.viewport.emit('scroll');
        s.flush();
    }
    assert.deepEqual(s.heightWrites, []);
    assert.equal(s.composer.style.height, '24px');
});

test('UIKit final geometry stays stable throughout noisy WebKit keyboard events', () => {
    const s = surface();
    const keyboard = { height: 525, fullHeight: 852, width: 393, duration: 0.25, curve: 7 };
    s.window.courtsideLayout.keyboardChanged(keyboard);
    assert.equal(s.properties.get('--viewport-height'), '525px');
    assert.equal(s.classes.has('keyboard-open'), true);
    for (const height of [800, 680, 510, 532, 525]) {
        s.viewport.height = s.window.innerHeight = height;
        s.viewport.emit('resize');
        s.flush();
        assert.equal(s.properties.get('--viewport-height'), '525px');
        assert.equal(s.properties.get('--unobscured-height'), '852px');
    }
    s.window.courtsideLayout.keyboardChanged({ ...keyboard, duration: 0 });
    assert.equal(s.timers.size, 1);
    s.finishTimers();
    s.flush();
    assert.equal(s.classes.has('keyboard-transition'), false);
    assert.equal(s.classes.has('keyboard-open'), true);
});

test('keyboard dismissal follows UIKit rather than resizing through every intermediate frame', () => {
    const s = surface();
    s.window.courtsideLayout.keyboardChanged({ height: 525, fullHeight: 852, width: 393, duration: 0.25 });
    s.window.courtsideLayout.keyboardChanged({ height: 852, fullHeight: 852, width: 393, duration: 0.25 });
    assert.equal(s.timers.size, 1);
    for (const height of [525, 680, 750, 852]) {
        s.viewport.height = s.window.innerHeight = height;
        s.viewport.emit('resize');
        s.flush();
        assert.equal(s.properties.get('--viewport-height'), '852px');
        assert.equal(s.classes.has('keyboard-open'), false);
    }
    s.finishTimers();
    s.flush();
    assert.equal(s.classes.has('keyboard-transition'), false);
});

test('rotation clears native geometry from the previous orientation', () => {
    const s = surface();
    s.window.courtsideLayout.keyboardChanged({ height: 525, fullHeight: 852, width: 393, duration: 0.25 });
    s.window.innerHeight = s.viewport.height = 393;
    s.window.innerWidth = s.viewport.width = 852;
    s.window.emit('orientationchange');
    s.flush();
    assert.equal(s.properties.get('--viewport-height'), '393px');
    assert.equal(s.classes.has('keyboard-open'), false);
    assert.equal(s.timers.size, 0);
    s.window.courtsideLayout.dispose();
    assert.equal(s.frames.size, 0);
});

test('the first textarea tap focuses without scrolling and retains default caret placement', () => {
    const s = surface();
    let focused;
    s.composer.focus = (options) => { focused = options; s.document.activeElement = s.composer; };
    s.composer.closest = () => null;
    let prevented = false;
    s.document.emit('pointerdown', { target: s.composer, preventDefault: () => { prevented = true; } });
    assert.equal(focused.preventScroll, true);
    assert.equal(prevented, false);
});

test('composer buttons keep input focus and outside controls keep normal behavior', () => {
    const s = surface();
    let prevented = 0;
    const buttonEvent = {
        target: { closest: () => true },
        preventDefault: () => prevented++,
    };
    s.document.emit('pointerdown', buttonEvent);
    assert.equal(prevented, 0);
    s.document.activeElement = s.composer;
    s.document.emit('pointerdown', buttonEvent);
    assert.equal(prevented, 1);
    s.document.emit('pointerdown', {
        target: { closest: () => null },
        preventDefault: () => prevented++,
    });
    assert.equal(prevented, 1);
});

test('call controls retain composer focus so a keyboard resize cannot swallow their first tap', () => {
    const s = surface();
    s.document.activeElement = s.composer;
    const control = {
        closest(selector) {
            return selector.split(',').some((part) => part.trim() === '.call-controls button') ? this : null;
        },
    };
    let prevented = 0;
    s.document.emit('pointerdown', { target: control, preventDefault: () => prevented++ });
    assert.equal(prevented, 1);
    assert.equal(s.document.activeElement, s.composer);
    s.document.activeElement = null;
    s.document.emit('pointerdown', { target: control, preventDefault: () => prevented++ });
    assert.equal(prevented, 1);
});

test('call-control feedback scrolls its chat container without panning the webview', () => {
    const s = surface();
    const message = { getBoundingClientRect: () => ({ top: 630, bottom: 660 }) };
    const lookup = s.document.getElementById;
    s.document.getElementById = (id) => id === 'control-message' ? message : lookup(id);
    s.chat.contains = (element) => element === message;
    s.chat.getBoundingClientRect = () => ({ top: 100, bottom: 500 });
    s.window.courtsideLayout.revealCallControl('control-message');
    s.flush();
    assert.equal(s.chat.scrolls[0].top, 168);
    assert.equal(s.properties.get('--viewport-top'), '0px');
    message.getBoundingClientRect = () => ({ top: 200, bottom: 230 });
    s.window.courtsideLayout.revealCallControl('control-message');
    s.flush();
    assert.equal(s.chat.scrolls.length, 1);
});

test('pinch zoom does not masquerade as a software keyboard', () => {
    const s = surface();
    s.viewport.scale = 2;
    s.viewport.height = 426;
    s.viewport.emit('resize');
    s.flush();
    assert.equal(s.properties.get('--viewport-height'), '852px');
    assert.equal(s.classes.has('keyboard-open'), false);
    s.viewport.scale = 1;
    s.viewport.height = 852;
    s.viewport.emit('resize');
    s.flush();
    assert.equal(s.properties.get('--viewport-height'), '852px');
});

test('fallback viewport and pre-stylesheet text measurements remain usable', () => {
    const s = surface({ visualViewport: false });
    s.window.innerHeight = 600;
    s.composer.maxHeight = 'none';
    s.composer.minHeight = 'auto';
    s.window.emit('resize');
    s.flush();
    assert.equal(s.properties.get('--viewport-height'), '600px');
    assert.equal(s.composer.style.height, '24px');
    assert.equal(s.classes.has('keyboard-open'), false);
});

test('reinstallation cancels stale frames and does not duplicate event listeners', () => {
    const s = surface();
    s.window.courtsideLayout.schedule();
    assert.equal(s.frames.size, 1);
    s.install();
    assert.equal(s.frames.size, 0);
    for (const target of [s.window, s.viewport, s.document]) {
        for (const listeners of target.listeners.values()) assert.equal(listeners.size, 1);
    }
    s.window.courtsideLayout.dispose();
    for (const target of [s.window, s.viewport, s.document]) {
        for (const listeners of target.listeners.values()) assert.equal(listeners.size, 0);
    }
});
