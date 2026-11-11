mod api;
mod contacts;
mod listen;
mod native;
mod session;
mod ui;
mod voice;
#[cfg(target_arch = "wasm32")]
mod web;

use dioxus::prelude::*;

const MAIN_CSS: Asset = asset!("/assets/main.css");

#[cfg(target_arch = "wasm32")]
fn main() {
    dioxus::launch(web::WebRoot);
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    // This is applied to WKWebView before any HTML exists. It closes the
    // native gray frame between LaunchScreen and the first DOM paint.
    let config = dioxus::mobile::Config::new().with_background_color((5, 6, 9, 255));
    #[cfg(target_os = "ios")]
    let config = {
        use dioxus::mobile::tao::platform::ios::{ValidOrientations, WindowBuilderExtIOS};

        // Constrain the native controller as well as the packaged orientation
        // list, so rotating the phone never resizes the chat or keyboard.
        config.with_window(
            dioxus::mobile::WindowBuilder::new()
                .with_title("Courtside")
                .with_valid_orientations(ValidOrientations::Portrait),
        )
    };

    dioxus::LaunchBuilder::mobile().with_cfg(config).launch(App);
}

#[component]
fn App() -> Element {
    let session_state = use_signal(session::load);
    let mut webview_ready = use_signal(|| false);
    let active_session = session_state();

    api::set_token(active_session.as_ref().map(|current| current.token.clone()));

    // Keep the native black launch surface visible until the external CSS and
    // first-frame images are ready. This prevents unstyled drawer markup from
    // appearing before its closed-state rules have loaded.
    use_future(move || async move {
        let _ = document::eval(include_str!("layout.js")).await;
        let _ = document::eval(
            r#"
await new Promise((resolve) => {
    const started = performance.now();
    const stylesReady = () => {
        const links = [...document.querySelectorAll('link[rel="stylesheet"]')];
        return links.length > 0 && links.every((link) => link.sheet);
    };
    const wait = () => {
        if (stylesReady() || performance.now() - started > 1500) {
            resolve();
        } else {
            requestAnimationFrame(wait);
        }
    };
    wait();
});

await Promise.all([...document.images].map(async (image) => {
    if (!image.complete) {
        await new Promise((resolve) => {
            image.addEventListener('load', resolve, { once: true });
            image.addEventListener('error', resolve, { once: true });
        });
    }
    if (image.decode) {
        await image.decode().catch(() => {});
    }
}));

if (document.fonts?.ready) {
    await document.fonts.ready;
}

await new Promise((resolve) =>
    requestAnimationFrame(() => requestAnimationFrame(resolve))
);
window.courtsideLayout?.schedule();
return true;
"#,
        )
        .await;
        native::hide_keyboard_accessory();
        webview_ready.set(true);
    });

    let launch_cover_style = if webview_ready() {
        "position:fixed;inset:0;z-index:2147483647;background:#050609;opacity:0;pointer-events:none;transition:opacity 240ms ease-out;will-change:opacity;"
    } else {
        "position:fixed;inset:0;z-index:2147483647;background:#050609;opacity:1;pointer-events:auto;transition:opacity 240ms ease-out;will-change:opacity;"
    };

    rsx! {
        document::Title { "Courtside" }
        document::Meta {
            name: "viewport",
            content: if cfg!(target_arch = "wasm32") { "width=device-width, initial-scale=1, viewport-fit=cover" } else { "width=device-width, initial-scale=1, maximum-scale=1, user-scalable=no, viewport-fit=cover" },
        }
        document::Stylesheet { href: MAIN_CSS }

        div {
            aria_hidden: "true",
            style: "position:fixed;inset:0;z-index:0;background:#050609;pointer-events:none;",
        }
        div {
            id: "courtside-webview",
            class: "courtside-webview",
            if let Some(current) = active_session {
                ui::SignedInGate { current, session_state }
            } else {
                ui::AuthFlow { session_state }
            }
        }
        div {
            aria_hidden: "true",
            style: launch_cover_style,
        }
    }
}
