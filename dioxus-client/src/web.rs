use dioxus::prelude::*;

const WEB_CSS: Asset = asset!("/assets/web.css");
const LOGO: Asset = asset!("/assets/CourtsideMark.png");

#[component]
pub fn WebRoot() -> Element {
    let path = use_signal(|| {
        web_sys::window()
            .and_then(|w| w.location().pathname().ok())
            .unwrap_or_default()
    });
    rsx! {
        document::Stylesheet { href: crate::MAIN_CSS }
        document::Stylesheet { href: WEB_CSS }
        document::Meta { name: "viewport", content: "width=device-width, initial-scale=1, viewport-fit=cover" }
        if path().starts_with("/app") {
            div { class: "web-app",
                a { class: "web-home-link", href: "/", "← Courtside home" }
                crate::App {}
            }
        } else if path().starts_with("/demo") {
            Demo {}
        } else {
            Landing {}
        }
    }
}

#[component]
fn Arrow() -> Element {
    rsx! { svg { width: "18", height: "18", view_box: "0 0 24 24", fill: "none", "aria-hidden": "true",
        path { d: "M5 12h14m-6-6 6 6-6 6", stroke: "currentColor", stroke_width: "1.7", stroke_linecap: "round", stroke_linejoin: "round" }
    } }
}

#[component]
fn Brand() -> Element {
    rsx! { a { class: "web-brand", href: "/", aria_label: "Courtside home",
        img { src: LOGO, width: "32", height: "32", alt: "" }
        span { "Courtside" }
    } }
}

#[component]
fn Landing() -> Element {
    let mut scenario = use_signal(|| 0_usize);
    let samples = [
        (
            "Dinner plans",
            "Restaurant reservation",
            "Book a table for two at Ember this Friday, around 7 pm.",
            "Ember",
            "Of course. We have a table at 7:15. Does that work?",
            "7:15 is perfect. Please book it for two.",
            "Table for two · Friday, 7:15 pm",
        ),
        (
            "Appointments",
            "Appointment scheduling",
            "Find the earliest available dental cleaning next week.",
            "Dental office",
            "We have an opening on Tuesday at 10 am.",
            "That works. What should they bring to the appointment?",
            "Appointment available · Tuesday, 10 am",
        ),
        (
            "Follow-ups",
            "Service follow-up",
            "Check whether my bike repair is ready for pickup.",
            "Repair shop",
            "Your bike is ready. We’re open until 6 today.",
            "Great. I’ll let them know they can pick it up today.",
            "Ready for pickup · Before 6 pm",
        ),
    ];
    let sample = samples[scenario()];
    rsx! {
        document::Title { "Courtside — Your calls, handled." }
        document::Meta { name: "description", content: "An AI phone assistant that makes calls for you. Follow the live transcript, listen in, take over, and keep the summary. Built with Rust and Dioxus." }
        main { class: "web-landing web-root",
            header { class: "web-nav web-container",
                Brand {}
                nav { aria_label: "Main navigation",
                    a { href: "#product", "Product" }
                    a { href: "#how-it-works", "How it works" }
                    a { href: "#project", "The build" }
                }
                div { class: "web-nav-actions",
                    a { class: "web-signin", href: "/app/?login=1", "Sign in" }
                    a { class: "web-button web-button-small", href: "/app/", "Open web app" Arrow {} }
                }
            }
            section { class: "web-hero web-container", id: "product",
                div { class: "web-hero-copy",
                    span { class: "web-eyebrow", i {} "YOUR AI PHONE ASSISTANT" }
                    h1 { "Your calls." br {} span { "Handled." } }
                    p { class: "web-hero-description", "A reservation. An appointment. That follow-up you keep putting off. Tell Courtside what you need, and let it make the call." }
                    div { class: "web-hero-actions",
                        a { class: "web-button", href: "/app/?signup=1", "Start in your browser" Arrow {} }
                        a { class: "web-button web-button-quiet", href: "/demo/", span { class: "web-play", "▶" } "Try the demo" }
                    }
                    p { class: "web-hero-footnote", "Follow along live. Step in whenever you need." }
                    div { class: "web-scenario-picker", aria_label: "Explore example calls",
                        for (index, sample) in samples.iter().enumerate() {
                            button {
                                class: if scenario() == index { "active" } else { "" },
                                aria_pressed: if scenario() == index { "true" } else { "false" },
                                onclick: move |_| scenario.set(index),
                                "{sample.0}"
                            }
                        }
                    }
                }
                div { class: "web-product-scene",
                    div { class: "web-scene-glow" }
                    div { class: "web-product-window",
                        div { class: "web-window-top", div { class: "web-window-dots", i {} i {} i {} } span { "Courtside" } span { class: "web-sample-label", "EXAMPLE" } }
                        div { class: "web-preview-body",
                            div { class: "web-preview-request", p { "{sample.2}" } }
                            div { class: "web-preview-assistant", img { src: LOGO, width: "25", height: "25", alt: "" } p { "I’ll handle it. You can follow the conversation here." } }
                            div { class: "web-preview-call",
                                div { class: "web-preview-call-top", div { span { class: "web-connected", i {} "CONNECTED" } h2 { "{sample.3}" } } div { class: "web-waveform", for i in 0..12 { i { style: "--bar:{i};" } } } }
                                div { class: "web-preview-transcript", div { small { "Other party" } p { "{sample.4}" } } div { small { "Courtside" } p { "{sample.5}" } } }
                                div { class: "web-preview-controls", span { "◧  End call" } span { "◖  Live listen" } span { "↗  Take over" } }
                            }
                            div { class: "web-preview-composer", span { "Plan or ask about a call…" } div { span { "+" } small { "GPT Live" } b { "↑" } } }
                        }
                    }
                    div { class: "web-outcome-note", span { class: "web-outcome-check", "✓" } div { small { "ONE LESS THING ON YOUR LIST" } strong { "{sample.6}" } } }
                }
            }
            div { class: "web-capabilities web-container", span { "REAL PHONE CALLS" } span { "LIVE TRANSCRIPTS" } span { "HUMAN TAKEOVER" } span { "CALL SUMMARIES" } }
            section { class: "web-how web-container", id: "how-it-works",
                div { class: "web-section-heading", span { class: "web-eyebrow", "FROM TO-DO TO TAKEN CARE OF" } h2 { "A little direction. A lot less doing." } p { "One conversation to plan the call, follow it, and see what happened." } }
                div { class: "web-steps",
                    article { span { class: "web-step-number", "01" } div { class: "web-step-art web-art-plan", span { "Table for two, Friday at 7." } span { "Any seating preferences?" } } h3 { "Tell it the plan." } p { "Share who to call, what to ask, and the details that matter. Courtside helps you get the instructions right." } }
                    article { span { class: "web-step-number", "02" } div { class: "web-step-art web-art-live", div { class: "web-live-ring", "↗" } span { i {} "Call in progress" } } h3 { "Let the call happen." } p { "Watch the live transcript and listen in. Take over the conversation from the app when you want to speak." } }
                    article { span { class: "web-step-number", "03" } div { class: "web-step-art web-art-summary", div { span { "✓" } strong { "Reservation confirmed" } } p { "Friday · 7:15 pm · 2 people" } } h3 { "Keep the outcome." } p { "The transcript and summary stay in your call history, ready when you need the details again." } }
                }
            }
            section { class: "web-control web-container",
                div { class: "web-control-visual",
                    crate::ui::LiquidAquarium {}
                    div { class: "web-control-orbit", span { "YOU" } }
                    div { class: "web-control-status", i {} "Your assistant is on the call" }
                }
                div { class: "web-control-copy", span { class: "web-eyebrow", "STAY IN THE LOOP" } h2 { "Off your plate.\nStill in your hands." } p { "You don’t have to disappear from the conversation. Follow every update, hear the call, or step in and speak for yourself." }
                    ul { li { span { "01" } "Read the conversation as it happens" } li { span { "02" } "Listen without interrupting" } li { span { "03" } "Take over, then hand it back" } }
                    a { class: "web-inline-link", href: "/demo/", "See the conversation in action" Arrow {} }
                }
            }
            section { class: "web-project web-container", id: "project",
                div { span { class: "web-eyebrow", "BEHIND THE BUILD" } h2 { "A real app.\nFrom interface to phone line." } p { "Courtside connects a shared Dioxus interface to a Rust service that bridges phone audio, AI, and your live conversation." } }
                div { class: "web-architecture",
                    div { class: "web-architecture-row", span { "INTERFACE" } strong { "Dioxus" } small { "Web + native iPhone app" } }
                    div { class: "web-architecture-row", span { "BACKEND" } strong { "Rust + Axum" } small { "Authenticated APIs and WebSocket streams" } }
                    div { class: "web-architecture-row", span { "VOICE" } strong { "Twilio + OpenAI Realtime" } small { "Phone audio, transcripts, and call control" } }
                    div { class: "web-architecture-row", span { "STATE" } strong { "SeaORM + SQLite" } small { "Accounts, conversations, and call history" } }
                }
            }
            section { class: "web-final-cta web-container", span { class: "web-eyebrow", "TAKE SOMETHING OFF YOUR LIST" } h2 { "What should we handle?" } p { "Give your next phone call to Courtside." } div { a { class: "web-button", href: "/app/?signup=1", "Open Courtside" Arrow {} } a { class: "web-button web-button-quiet", href: "/demo/", "Explore the demo" } } }
            footer { class: "web-footer web-container", Brand {} p { "Your calls. Higher ground." } a { href: "#project", "Built with Rust & Dioxus" } }
        }
    }
}

#[component]
fn Demo() -> Element {
    let mut step = use_signal(|| 0_usize);
    let mut takeover = use_signal(|| false);
    let mut listening = use_signal(|| false);
    let mut prompt =
        use_signal(|| "Book a table for two at Ember this Friday, around 7 pm.".to_string());
    use_drop(|| {
        let _ = document::eval("window.speechSynthesis?.cancel();");
    });
    use_effect(move || {
        if !listening() || step() > 5 {
            let _ = document::eval("window.speechSynthesis?.cancel();");
        }
    });
    let lines = [
        ("Other party", "Thanks for calling Ember. How can I help?"),
        (
            "Courtside",
            "Hello! I’m calling to reserve a table for two this Friday, around 7 pm.",
        ),
        ("Other party", "We have 7:15 available. Would that work?"),
        (
            "Courtside",
            "7:15 is perfect. Please confirm the reservation for two.",
        ),
        ("Other party", "You’re all set. We’ll see you Friday!"),
    ];
    rsx! {
        document::Title { "Courtside — Interactive demo" }
        main { class: "web-demo web-root",
            aside { class: "web-demo-sidebar", Brand {} a { class: "web-demo-nav active", href: "/demo/", "◇  Assistant" } button { class: "web-demo-nav", onclick: move |_| { step.set(0); takeover.set(false); listening.set(false); }, "＋  New example" } div { class: "web-demo-sidebar-bottom", p { "Explore the experience with a sample conversation." } a { class: "web-button", href: "/app/?signup=1", "Open the real app" Arrow {} } } }
            section { class: "web-demo-main",
                header { class: "web-demo-header", span { class: "web-connected", i {} "GUIDED DEMO" } a { href: "/", "Back to home ↗" } }
                div { class: "web-demo-content",
                    if step() == 0 {
                        div { class: "web-demo-empty", crate::ui::LiquidAquarium {} span { class: "web-eyebrow", "A LITTLE PREVIEW" } h1 { "What should I handle?" } p { "Try a reservation example and see how a call unfolds." } }
                    } else {
                        div { class: "web-demo-request", "{prompt}" }
                        div { class: "web-demo-response", img { src: LOGO, width: "28", height: "28", alt: "Courtside" } p { "Here’s a sample of how I would handle a restaurant reservation. This demo doesn’t place a real call." } }
                        article { class: "web-demo-call",
                            header { div { span { class: "web-connected", i {} if step() > 5 { "COMPLETED" } else if takeover() { "YOU’RE SPEAKING" } else { "EXAMPLE CALL" } } h2 { "Ember restaurant" } p { "Reservation · Friday · Two people" } } span { class: "web-demo-time", if step() > 5 { "Summary ready" } else { "Sample transcript" } } }
                            div { class: "web-demo-transcript", aria_live: "polite",
                                for (speaker, text) in lines.iter().take(step().min(5)) {
                                    div { small { "{speaker}" } p { "{text}" } }
                                }
                            }
                            if step() <= 5 {
                                div { class: "web-demo-call-controls",
                                    button { class: "web-demo-end", onclick: move |_| { step.set(6); listening.set(false); }, "◧ End example" }
                                    button { class: if listening() { "active" } else { "" }, onclick: move |_| {
                                        listening.set(!listening());
                                        let script = if listening() { "if (window.speechSynthesis) { speechSynthesis.cancel(); speechSynthesis.speak(new SpeechSynthesisUtterance('This is a sample conversation. Thanks for calling Ember. We have a table for two at seven fifteen this Friday.')); }" } else { "window.speechSynthesis?.cancel();" };
                                        let _ = document::eval(script);
                                    }, if listening() { "◖ Stop sample audio" } else { "◖ Listen to sample" } }
                                    button { class: if takeover() { "active" } else { "" }, onclick: move |_| takeover.set(!takeover()), if takeover() { "↩ Return to AI" } else { "↗ Take over" } }
                                }
                                if takeover() { p { class: "web-demo-note", "You’d now speak through your microphone in the real app. Here, you can explore the controls without calling anyone." } }
                                button { class: "web-inline-link web-demo-next", onclick: move |_| step.set(step() + 1), if step() == 5 { "Show the outcome" } else { "Continue the conversation" } Arrow {} }
                            } else {
                                div { class: "web-demo-summary", span { class: "web-eyebrow", "CALL SUMMARY" } h3 { "Dinner plans, handled." } p { "A table for two is confirmed at Ember for Friday at 7:15 pm. The restaurant confirmed the reservation." } span { "✓ Friday, 7:15 pm   ·   2 guests" } }
                            }
                        }
                    }
                }
                div { class: "web-demo-dock",
                    if step() == 0 {
                        form { class: "composer", onsubmit: move |event| { event.prevent_default(); if !prompt().trim().is_empty() { step.set(1); } },
                            textarea { rows: "2", aria_label: "Demo call request", value: "{prompt}", oninput: move |event| prompt.set(event.value()), maxlength: "500" }
                            div { class: "composer-actions", small { "Restaurant reservation example" } button { class: "send-button", r#type: "submit", aria_label: "Start the guided demo", disabled: prompt().trim().is_empty(), "↑" } }
                        }
                    } else {
                        a { class: "web-button", href: "/app/?signup=1", "Ready for a real call? Open Courtside" Arrow {} }
                    }
                    p { class: "web-demo-disclaimer", "Illustrative demo · No account or real phone call required" }
                }
            }
        }
    }
}
