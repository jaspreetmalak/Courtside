// UIKit supplies the keyboard's final geometry and animation duration on iOS.
// VisualViewport remains the fallback and compensates for any WebKit panning.
(() => {
    window.courtsideLayout?.dispose();

    const root = document.documentElement;
    const viewport = window.visualViewport;
    let pendingFrame = 0;
    let layoutWidth = window.innerWidth;
    let unobscuredHeight = Math.max(window.innerHeight, root.clientHeight || 0);
    let nativeKeyboard = null;
    let measuredComposer = null;
    let measuredValue = '';
    let measuredWidth = 0;
    let contentHeight = 24;
    let needsMeasurement = true;
    let animations = [];
    let animationFrame = 0;
    let animationTimer = 0;

    const notifyLayout = () => window.dispatchEvent(new Event('courtside:layout'));
    const stopAnimation = () => {
        cancelAnimationFrame(animationFrame);
        clearTimeout(animationTimer);
        animationFrame = animationTimer = 0;
        for (const animation of animations) animation.cancel();
        animations = [];
        root.classList.toggle('keyboard-transition', false);
    };

    const update = () => {
        pendingFrame = 0;
        // Pinch zoom is handled by the browser; it is not a keyboard resize.
        if (viewport && Math.abs(viewport.scale - 1) > 0.01) return;

        const chat = document.getElementById('courtside-chat-content');
        const followChat = chat && chat.scrollHeight - chat.clientHeight - chat.scrollTop < 48;
        if (nativeKeyboard && Math.abs(nativeKeyboard.width - window.innerWidth) > 1) {
            nativeKeyboard = null;
            stopAnimation();
        }
        const height = nativeKeyboard?.height ?? viewport?.height ?? window.innerHeight;
        const width = viewport?.width ?? window.innerWidth;
        // WKWebView can shrink innerHeight along with visualViewport.height.
        // Retain the full height for this orientation rather than comparing
        // two values that both become the keyboard's available height.
        const layoutHeight = Math.max(window.innerHeight, root.clientHeight || 0);
        if (layoutWidth !== window.innerWidth) {
            layoutWidth = window.innerWidth;
            unobscuredHeight = layoutHeight;
        } else {
            unobscuredHeight = Math.max(unobscuredHeight, layoutHeight, height);
        }
        if (nativeKeyboard) unobscuredHeight = nativeKeyboard.fullHeight;
        root.style.setProperty('--unobscured-height', `${unobscuredHeight}px`);
        root.style.setProperty('--viewport-height', `${height}px`);
        root.style.setProperty('--viewport-width', `${width}px`);
        root.style.setProperty('--viewport-top', `${viewport?.offsetTop ?? 0}px`);
        root.style.setProperty('--viewport-left', `${viewport?.offsetLeft ?? 0}px`);
        root.classList.toggle('keyboard-open', Boolean(nativeKeyboard || viewport) && unobscuredHeight - height > 120);

        const composer = document.getElementById('courtside-composer-input');
        if (composer) {
            const styles = getComputedStyle(composer);
            const maxHeight = parseFloat(styles.maxHeight) || 144;
            const value = composer.value ?? '';
            const fieldWidth = composer.clientWidth ?? width;
            if (needsMeasurement || measuredComposer !== composer
                || measuredValue !== value || measuredWidth !== fieldWidth) {
                // Only remeasure changed text/width. Collapsing the focused
                // textarea on every viewport frame makes WebKit reveal its
                // caret repeatedly and fights the keyboard animation.
                composer.style.height = '0px';
                contentHeight = Math.max(parseFloat(styles.minHeight) || 24, composer.scrollHeight);
                measuredComposer = composer;
                measuredValue = value;
                measuredWidth = fieldWidth;
                needsMeasurement = false;
            }
            const fieldHeight = `${Math.min(contentHeight, maxHeight)}px`;
            if (composer.style.height !== fieldHeight) composer.style.height = fieldHeight;
            const overflow = contentHeight > maxHeight ? 'auto' : 'hidden';
            if (composer.style.overflowY !== overflow) composer.style.overflowY = overflow;
        }
        if (followChat) chat.scrollTop = chat.scrollHeight;
        notifyLayout();
    };

    const schedule = (measure = false) => {
        if (measure === true) needsMeasurement = true;
        if (!pendingFrame) pendingFrame = requestAnimationFrame(update);
    };
    const onInput = (event) => {
        if (event.target.id === 'courtside-composer-input') schedule(true);
    };
    const onPointerDown = (event) => {
        if (event.target.id === 'courtside-composer-input'
            && document.activeElement !== event.target) {
            // Keep WebKit from starting its own scroll-to-focus animation.
            // The default pointer action can still place the caret normally.
            event.target.focus({ preventScroll: true });
        }
        if (document.activeElement?.id === 'courtside-composer-input'
            && event.target.closest('.composer-actions button, .call-controls button')) {
            // Handle this synchronously before WebKit blurs the text field.
            // A round trip through the native renderer can arrive too late.
            event.preventDefault();
        }
    };
    window.addEventListener('resize', schedule);
    window.addEventListener('orientationchange', schedule);
    viewport?.addEventListener('resize', schedule);
    viewport?.addEventListener('scroll', schedule);
    document.addEventListener('input', onInput);
    document.addEventListener('pointerdown', onPointerDown);
    window.courtsideLayout = {
        schedule,
        keyboardChanged(data) {
            if (!Number.isFinite(data.height) || !Number.isFinite(data.fullHeight)
                || data.height <= 0 || data.fullHeight <= 0
                || !Number.isFinite(data.width) || data.width <= 0) return;
            if (nativeKeyboard && Math.abs(nativeKeyboard.height - data.height) < 0.5
                && nativeKeyboard.width === data.width) {
                nativeKeyboard = data;
                schedule();
                return;
            }
            const shell = document.getElementById('courtside-webview');
            const targets = [...document.querySelectorAll(
                '.chat-dock, .empty-chat .open-water, .empty-chat .assistant-hero h1, .empty-chat .assistant-hero p'
            )];
            const relativeFrame = (element) => {
                const rect = element.getBoundingClientRect();
                const origin = shell?.getBoundingClientRect() ?? { x: 0, y: 0 };
                return { x: rect.x - origin.x, y: rect.y - origin.y };
            };
            // Capture the current presentation before cancelling an interrupted
            // transition, so quickly opening/closing never snaps back first.
            const before = targets.map(relativeFrame);
            stopAnimation();
            cancelAnimationFrame(pendingFrame);
            pendingFrame = 0;
            nativeKeyboard = data;
            const duration = window.matchMedia?.('(prefers-reduced-motion: reduce)')?.matches
                ? 0 : Math.min(1000, Math.max(0, (data.duration || 0) * 1000));
            if (duration > 0) root.classList.toggle('keyboard-transition', true);
            update();
            const easing = [
                'ease-in-out', 'ease-in', 'ease-out', 'linear',
            ][data.curve] ?? 'cubic-bezier(0.25, 0.1, 0.25, 1)';
            if (duration > 0) {
                targets.forEach((element, index) => {
                    const after = relativeFrame(element);
                    const x = before[index].x - after.x;
                    const y = before[index].y - after.y;
                    if (element.animate && (Math.abs(x) > 0.5 || Math.abs(y) > 0.5)) {
                        animations.push(element.animate([
                            { transform: `translate(${x}px, ${y}px)` },
                            { transform: 'translate(0px, 0px)' },
                        ], { duration, easing }));
                    }
                });
                // Native menu hit targets must follow the animated DOM labels.
                const trackAnimation = () => {
                    notifyLayout();
                    animationFrame = requestAnimationFrame(trackAnimation);
                };
                animationFrame = requestAnimationFrame(trackAnimation);
                animationTimer = setTimeout(() => {
                    stopAnimation();
                    schedule();
                }, duration + 34);
            }
            notifyLayout();
        },
        scrollToEnd(id, behavior = 'auto') {
            // scrollIntoView also scrolls the WKWebView and can pan the header
            // out of sight. Scroll only the requested conversation container.
            requestAnimationFrame(() => {
                const container = document.getElementById(id);
                container?.scrollTo({ top: container.scrollHeight, behavior });
            });
        },
        revealCallControl(id) {
            requestAnimationFrame(() => {
                const message = document.getElementById(id);
                const chat = document.getElementById('courtside-chat-content');
                if (!message || !chat?.contains(message)) return;
                const bounds = chat.getBoundingClientRect();
                const rect = message.getBoundingClientRect();
                const offset = rect.bottom > bounds.bottom
                    ? rect.bottom - bounds.bottom + 8
                    : rect.top < bounds.top ? rect.top - bounds.top - 8 : 0;
                if (offset) chat.scrollTo({ top: chat.scrollTop + offset, behavior: 'auto' });
            });
        },
        dispose() {
            stopAnimation();
            cancelAnimationFrame(pendingFrame);
            window.removeEventListener('resize', schedule);
            window.removeEventListener('orientationchange', schedule);
            viewport?.removeEventListener('resize', schedule);
            viewport?.removeEventListener('scroll', schedule);
            document.removeEventListener('input', onInput);
            document.removeEventListener('pointerdown', onPointerDown);
        },
    };
    update();
})();
