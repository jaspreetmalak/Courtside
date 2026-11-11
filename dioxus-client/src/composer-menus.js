// Transparent UIKit buttons own the two separate system menus. Match their
// hit targets to the DOM controls, relative to the visible app shell.
const owner = __OWNER__;
window.courtsideComposerMenus ??= new Map();
await new Promise((resolve) => {
    let pendingFrame = 0;
    let lastFrames = '';
    let disposed = false;
    let mounted = false;
    let observedComposer = null;
    const viewport = window.visualViewport;
    const frame = (element, origin) => {
        const rect = element.getBoundingClientRect();
        return {
            // WebKit can already account for keyboard panning in client rects.
            // Measuring both rectangles cancels that origin without subtracting
            // visualViewport.offsetTop a second time.
            x: rect.x - origin.x,
            y: rect.y - origin.y,
            width: rect.width,
            height: rect.height,
        };
    };
    const sendFrames = () => {
        pendingFrame = 0;
        if (disposed) return;
        const tools = document.getElementById('courtside-tools-button');
        const model = document.getElementById('courtside-model-button');
        const shell = document.getElementById('courtside-webview');
        if (!tools || !model || !shell) {
            if (!mounted) {
                schedule();
                return;
            }
            dispose();
            return;
        }
        mounted = true;
        const composer = document.querySelector('.composer');
        if (composer && composer !== observedComposer) {
            resizeObserver.disconnect();
            resizeObserver.observe(composer);
            observedComposer = composer;
        }
        const origin = shell.getBoundingClientRect();
        const frames = {
            tools: frame(tools, origin),
            model: frame(model, origin),
            visible: !document.querySelector('.drawer-overlay.open, .startup-input-guard'),
            tools_enabled: !tools.disabled,
            model_enabled: !model.disabled,
        };
        const serialized = JSON.stringify(frames);
        if (serialized !== lastFrames) {
            lastFrames = serialized;
            dioxus.send(frames);
        }
    };
    const schedule = () => {
        if (!disposed && !pendingFrame) pendingFrame = requestAnimationFrame(sendFrames);
    };
    const resizeObserver = new ResizeObserver(schedule);
    const mutationObserver = new MutationObserver(schedule);
    const dispose = () => {
        if (disposed) return;
        disposed = true;
        cancelAnimationFrame(pendingFrame);
        resizeObserver.disconnect();
        mutationObserver.disconnect();
        window.removeEventListener('resize', schedule);
        window.removeEventListener('courtside:layout', schedule);
        viewport?.removeEventListener('resize', schedule);
        viewport?.removeEventListener('scroll', schedule);
        window.courtsideComposerMenus.delete(owner);
        dioxus.send(null);
        resolve();
    };
    window.courtsideComposerMenus.set(owner, dispose);
    mutationObserver.observe(document.body, {
        childList: true,
        subtree: true,
        attributes: true,
        attributeFilter: ['class', 'disabled'],
    });
    window.addEventListener('resize', schedule);
    // The viewport handler also resizes the textarea and composer. Measure
    // after it finishes, even if WebKit's viewport event arrived first.
    window.addEventListener('courtside:layout', schedule);
    viewport?.addEventListener('resize', schedule);
    viewport?.addEventListener('scroll', schedule);
    schedule();
});
