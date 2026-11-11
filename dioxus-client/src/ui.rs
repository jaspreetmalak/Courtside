use chrono::Utc;
use dioxus::prelude::*;
use shared::{
    AccountStatusResponse, AccountTier, AvailableNumber, Call, CallAnswerTarget,
    CallDetailResponse, CallHandoffRequest, CallMode, CallPreferences, CallQuestion, CallStatus,
    CallVoice, ChatActivity, ChatMessage, ChatRole, ChatSummary, HistorySettingsResponse,
    ServerEvent, Speaker, UserProfile,
};
use uuid::Uuid;

use crate::api;
use crate::listen;
use crate::session::{self, Session};
use crate::voice;

const COURTSIDE_MARK: Asset = asset!("/assets/CourtsideMark.png");

#[derive(Clone, PartialEq)]
struct ChatTurnActivity {
    chat_id: Uuid,
    request_id: Uuid,
    activity: Option<ChatActivity>,
}

const CALL_VOICES: [CallVoice; 6] = [
    CallVoice::Marin,
    CallVoice::Cedar,
    CallVoice::Coral,
    CallVoice::Sage,
    CallVoice::Verse,
    CallVoice::Willow,
];

fn voice_label(voice: CallVoice) -> &'static str {
    match voice {
        CallVoice::Marin => "Marin",
        CallVoice::Cedar => "Cedar",
        CallVoice::Coral => "Coral",
        CallVoice::Sage => "Sage",
        CallVoice::Verse => "Verse",
        CallVoice::Willow => "Willow",
    }
}

fn voice_note(voice: CallVoice) -> &'static str {
    match voice {
        CallVoice::Marin => "Natural",
        CallVoice::Cedar => "Grounded",
        CallVoice::Coral => "Clear",
        CallVoice::Sage => "Balanced",
        CallVoice::Verse => "Expressive",
        CallVoice::Willow => "Gentle",
    }
}

fn normalize_caller_id_input(value: &str) -> String {
    let digits = value
        .chars()
        .filter(char::is_ascii_digit)
        .collect::<String>();
    match digits.len() {
        10 => format!("+1{digits}"),
        11 if digits.starts_with('1') => format!("+{digits}"),
        _ if value.trim().starts_with('+') => format!("+{digits}"),
        _ => value.trim().to_string(),
    }
}

fn strip_trailing_tool_command(value: &str) -> (String, bool) {
    let trimmed = value.trim_end();
    let token_start = trimmed
        .char_indices()
        .rev()
        .find_map(|(index, character)| {
            character
                .is_whitespace()
                .then_some(index + character.len_utf8())
        })
        .unwrap_or(0);
    let token = &trimmed[token_start..];
    let normalized = token.to_ascii_lowercase();
    let is_command = token.starts_with('@')
        || normalized == "/"
        || normalized.starts_with("/contact")
        || normalized.starts_with("/model");
    if is_command {
        (trimmed[..token_start].trim_end().to_string(), true)
    } else {
        (trimmed.to_string(), false)
    }
}

fn trailing_contact_query(value: &str) -> Option<String> {
    value
        .split_whitespace()
        .last()
        .and_then(|token| token.strip_prefix('@'))
        .map(str::to_string)
}

fn draft_with_contact(value: &str, contact: &crate::contacts::ContactSelection) -> String {
    let name = contact.name.trim();
    let phone = contact.phone_number.trim();
    let recipient = if name.is_empty() {
        phone.to_string()
    } else {
        format!("{name} at {phone}")
    };
    let (base, had_command) = strip_trailing_tool_command(value.trim());
    if base.is_empty() {
        format!("Call {recipient} about ")
    } else if had_command {
        format!("{base} {recipient}")
    } else {
        format!("{base}. Call {recipient}.")
    }
}

#[component]
fn VoicePicker(selected: CallVoice, on_select: EventHandler<CallVoice>) -> Element {
    rsx! {
        div { class: "voice-picker", role: "radiogroup", aria_label: "Assistant voice",
            for voice in CALL_VOICES {
                button {
                    r#type: "button",
                    class: if voice == selected { "voice-option selected" } else { "voice-option" },
                    role: "radio",
                    aria_checked: if voice == selected { "true" } else { "false" },
                    onclick: move |_| on_select.call(voice),
                    span { class: "voice-orb" }
                    span { class: "voice-copy",
                        strong { "{voice_label(voice)}" }
                        small { "{voice_note(voice)}" }
                    }
                    span { class: "voice-selected" }
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthScreen {
    Welcome,
    Login,
    Signup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pane {
    Assistant,
    Calls,
    Forwarding,
    Account,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallFilter {
    All,
    Active,
    Completed,
}

#[derive(Debug, Clone, PartialEq)]
struct LiveTranscriptLine {
    speaker: Speaker,
    text: String,
    start_ms: i64,
    end_ms: i64,
}

#[derive(Debug, Clone, PartialEq)]
struct LiveCallTranscript {
    call_id: Uuid,
    status: CallStatus,
    mode: CallMode,
    control_message: String,
    pending_question: Option<CallQuestion>,
    remembering: bool,
    lines: Vec<LiveTranscriptLine>,
}

impl LiveCallTranscript {
    fn new(call_id: Uuid, status: CallStatus) -> Self {
        Self {
            call_id,
            status,
            mode: CallMode::Assistant,
            control_message: String::new(),
            pending_question: None,
            remembering: false,
            lines: Vec::new(),
        }
    }

    fn from_detail(detail: CallDetailResponse) -> Self {
        let mode = detail.mode;
        let lines = detail
            .transcript
            .into_iter()
            .enumerate()
            .map(|(index, segment)| LiveTranscriptLine {
                speaker: segment.speaker,
                text: segment.text,
                start_ms: index as i64,
                end_ms: index as i64,
            })
            .collect();
        Self {
            call_id: detail.call.id,
            status: detail.call.status,
            mode,
            control_message: detail.control_message,
            pending_question: detail.pending_question,
            remembering: detail.remembering,
            lines,
        }
    }

    fn refresh(&mut self, detail: CallDetailResponse) {
        if self.call_id != detail.call.id {
            return;
        }
        self.status = detail.call.status;
        self.mode = detail.mode;
        self.control_message = detail.control_message.clone();
        self.pending_question = detail.pending_question.clone();
        self.remembering = detail.remembering;
        // Active deltas have not all been persisted yet. Only replace the
        // transcript with the final snapshot after the call has ended.
        if matches!(
            detail.call.status,
            CallStatus::Completed | CallStatus::Failed
        ) {
            *self = Self::from_detail(detail);
        }
    }

    fn push_delta(&mut self, speaker: Speaker, delta: String, start_ms: i64, end_ms: i64) {
        if delta.is_empty() {
            return;
        }
        if let Some(last) = self.lines.last_mut() {
            if last.speaker == speaker && start_ms <= last.end_ms + 1_200 {
                last.text.push_str(&delta);
                last.end_ms = last.end_ms.max(end_ms);
                return;
            }
        }
        self.lines.push(LiveTranscriptLine {
            speaker,
            text: delta,
            start_ms,
            end_ms,
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IconKind {
    Menu,
    Close,
    Chat,
    Phone,
    User,
    Contacts,
    Plus,
    ArrowUp,
    ArrowRight,
    Clock,
    File,
    Logout,
    Location,
    Search,
    ChevronLeft,
    Headphones,
    Stop,
}

#[component]
pub fn AuthFlow(mut session_state: Signal<Option<Session>>) -> Element {
    let mut screen = use_signal(|| {
        #[cfg(target_arch = "wasm32")]
        if let Some(window) = web_sys::window() {
            let query = window.location().search().unwrap_or_default();
            if query.contains("login=1") {
                return AuthScreen::Login;
            }
            if query.contains("signup=1") {
                return AuthScreen::Signup;
            }
        }
        AuthScreen::Welcome
    });

    match screen() {
        AuthScreen::Welcome => rsx! {
            main { class: "auth-page welcome-page",
                div { class: "auth-glow" }
                div { class: "auth-content",
                    div { class: "brand-lockup",
                        Logo { size: 48 }
                        div {
                            h1 { "Courtside" }
                            p { "Your calls. Higher ground." }
                        }
                    }
                    div { class: "welcome-copy",
                        span { class: "eyebrow", "AI PHONE ASSISTANT" }
                        h2 { "You ask.\nCourtside calls." }
                        p { "Handle reservations, appointments, support calls, and follow-ups without waiting on hold." }
                    }
                    LiquidAquarium {}
                    div { class: "auth-actions",
                        button {
                            class: "primary-button",
                            onclick: move |_| screen.set(AuthScreen::Signup),
                            "Create account"
                            Icon { kind: IconKind::ArrowRight, size: 20 }
                        }
                        button {
                            class: "text-button",
                            onclick: move |_| screen.set(AuthScreen::Login),
                            "I already have an account"
                        }
                    }
                }
            }
        },
        AuthScreen::Login => rsx! {
            AuthForm {
                mode: AuthScreen::Login,
                session_state,
                on_back: move |_| screen.set(AuthScreen::Welcome),
                on_switch: move |_| screen.set(AuthScreen::Signup),
            }
        },
        AuthScreen::Signup => rsx! {
            AuthForm {
                mode: AuthScreen::Signup,
                session_state,
                on_back: move |_| screen.set(AuthScreen::Welcome),
                on_switch: move |_| screen.set(AuthScreen::Login),
            }
        },
    }
}

#[component]
fn AuthForm(
    mode: AuthScreen,
    mut session_state: Signal<Option<Session>>,
    on_back: EventHandler<()>,
    on_switch: EventHandler<()>,
) -> Element {
    let signup = mode == AuthScreen::Signup;
    let mut signup_step = use_signal(|| 0_u8);
    let mut name = use_signal(String::new);
    let mut email = use_signal(String::new);
    let mut password = use_signal(String::new);
    let mut voice = use_signal(CallVoice::default);
    let mut announce_courtside = use_signal(|| true);
    let mut custom_instructions = use_signal(String::new);
    let mut message = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let preferences_step = signup && signup_step() == 1;

    let mut submit = move || {
        if busy() {
            return;
        }
        let email_value = email().trim().to_lowercase();
        let password_value = password();
        let name_value = name().trim().to_string();
        if email_value.is_empty() || password_value.len() < 8 || (signup && name_value.is_empty()) {
            message.set("Enter a valid email and a password with at least 8 characters.".into());
            return;
        }
        if signup && signup_step() == 0 {
            message.set(String::new());
            signup_step.set(1);
            return;
        }

        busy.set(true);
        message.set(if signup {
            "Finishing your setup…".into()
        } else {
            "Signing you in…".into()
        });
        let selected_voice = voice();
        let announce = announce_courtside();
        let instructions = custom_instructions();
        spawn(async move {
            let response = if signup {
                api::signup(
                    email_value,
                    password_value,
                    name_value,
                    selected_voice,
                    announce,
                    instructions,
                )
                .await
            } else {
                api::login(email_value, password_value).await
            };
            match response {
                Ok(auth) => {
                    let next = Session {
                        token: auth.token,
                        user: auth.user,
                    };
                    api::set_token(Some(next.token.clone()));
                    session::save(&next);
                    session_state.set(Some(next));
                }
                Err(error) => {
                    message.set(error);
                    busy.set(false);
                }
            }
        });
    };

    rsx! {
        main { class: if preferences_step { "auth-page form-page preferences-onboarding" } else { "auth-page form-page" },
            div { class: "auth-glow" }
            div { class: "form-shell",
                button {
                    class: "circle-button back-button",
                    aria_label: "Back",
                    onclick: move |_| {
                        if signup && signup_step() == 1 {
                            message.set(String::new());
                            signup_step.set(0);
                        } else {
                            on_back.call(());
                        }
                    },
                    Icon { kind: IconKind::ChevronLeft, size: 23 }
                }
                div { class: "form-brand", Logo { size: 54 } }
                span { class: "eyebrow",
                    if preferences_step {
                        "MAKE IT YOURS"
                    } else if signup {
                        "GET STARTED"
                    } else {
                        "WELCOME BACK"
                    }
                }
                h1 {
                    if preferences_step {
                        "Choose your assistant"
                    } else if signup {
                        "Create your account"
                    } else {
                        "Sign in to Courtside"
                    }
                }
                p { class: "form-intro",
                    if preferences_step {
                        "Set how Courtside should sound and introduce itself. You can change this anytime."
                    } else if signup {
                        "Your assistant is a few details away."
                    } else {
                        "Continue handling calls without the hold music."
                    }
                }
                form {
                    class: "auth-form",
                    onsubmit: move |event| {
                        event.prevent_default();
                        submit();
                    },
                    if preferences_step {
                        div { class: "onboarding-preference",
                            span { class: "preference-kicker", "VOICE" }
                            h2 { "How should Courtside sound?" }
                            VoicePicker {
                                selected: voice(),
                                on_select: move |selected| {
                                    voice.set(selected);
                                    message.set(String::new());
                                },
                            }
                        }
                        div { class: "onboarding-preference preference-row",
                            div {
                                span { class: "preference-kicker", "INTRODUCTION" }
                                strong { "Identify Courtside" }
                                p { "Say Courtside is calling on your behalf before the purpose." }
                            }
                            button {
                                r#type: "button",
                                class: if announce_courtside() { "preference-switch on" } else { "preference-switch" },
                                role: "switch",
                                aria_checked: if announce_courtside() { "true" } else { "false" },
                                aria_label: "Use the Courtside introduction",
                                onclick: move |_| {
                                    announce_courtside.set(!announce_courtside());
                                    message.set(String::new());
                                },
                                span {}
                            }
                        }
                        p { class: "greeting-preview",
                            if announce_courtside() {
                                "“Hello, this is the Courtside assistant calling on your behalf.”"
                            } else {
                                "Calls will begin directly with their purpose."
                            }
                        }
                        label { class: "onboarding-instructions",
                            "Default call style"
                            textarea {
                                maxlength: "800",
                                rows: "3",
                                placeholder: "Optional: Keep calls concise, speak slowly, confirm important details…",
                                value: "{custom_instructions}",
                                oninput: move |event| {
                                    custom_instructions.set(event.value());
                                    message.set(String::new());
                                },
                            }
                            small { "These instructions apply to future calls unless a specific request overrides them." }
                        }
                    } else {
                        if signup {
                            label {
                                "Name"
                                input {
                                    r#type: "text",
                                    autocomplete: "name",
                                    enterkeyhint: "next",
                                    placeholder: "Your name",
                                    value: "{name}",
                                    oninput: move |event| name.set(event.value()),
                                }
                            }
                        }
                        label {
                            "Email"
                            input {
                                r#type: "email",
                                autocomplete: "email",
                                inputmode: "email",
                                enterkeyhint: "next",
                                placeholder: "you@example.com",
                                value: "{email}",
                                oninput: move |event| email.set(event.value()),
                            }
                        }
                        label {
                            "Password"
                            input {
                                r#type: "password",
                                autocomplete: if signup { "new-password" } else { "current-password" },
                                enterkeyhint: if signup { "next" } else { "done" },
                                placeholder: "At least 8 characters",
                                value: "{password}",
                                oninput: move |event| password.set(event.value()),
                            }
                        }
                    }
                    if !message().is_empty() {
                        p { class: "form-message", "{message}" }
                    }
                    button {
                        class: "primary-button",
                        r#type: "submit",
                        disabled: busy(),
                        if busy() {
                            "Please wait…"
                        } else if preferences_step {
                            "Finish setup"
                        } else if signup {
                            "Continue"
                        } else {
                            "Sign in"
                        }
                        Icon { kind: IconKind::ArrowRight, size: 20 }
                    }
                }
                if !preferences_step {
                    button {
                        class: "text-button",
                        onclick: move |_| on_switch.call(()),
                        if signup { "Already have an account? Sign in" } else { "New to Courtside? Create account" }
                    }
                }
            }
        }
    }
}

#[component]
pub fn SignedInGate(current: Session, mut session_state: Signal<Option<Session>>) -> Element {
    let mut revision = use_signal(|| 0_u32);
    // This signal lives above AppShell so resolving the account request cannot
    // accidentally create a second chat. A new SignedInGate is mounted on the
    // next cold launch, which starts one new conversation for that app session.
    let launch_chat_initialized = use_signal(|| false);
    let account = use_resource(move || {
        let _ = revision();
        async move { api::account_status().await }
    });

    match account() {
        None => rsx! {
            AppShell {
                current: current.clone(),
                session_state,
                startup_pending: true,
                launch_chat_initialized,
            }
        },
        Some(Err(error)) => rsx! {
            SetupMessage {
                title: "We couldn’t finish setup",
                body: error,
                primary: "Try again",
                on_primary: move |_| *revision.write() += 1,
                session_state,
            }
        },
        // Browser users can plan calls before verifying caller ID. The server
        // still enforces verification and entitlements when a call is started.
        Some(Ok(_)) if cfg!(target_arch = "wasm32") => rsx! {
            AppShell { current, session_state, startup_pending: false, launch_chat_initialized }
        },
        Some(Ok(status)) if status.app_attest_required && !status.device_attested => rsx! {
            SetupMessage {
                title: "Device verification required",
                body: "The Dioxus App Attest bridge has not been enabled for this development build.".to_string(),
                primary: "Check again",
                on_primary: move |_| *revision.write() += 1,
                session_state,
            }
        },
        Some(Ok(status)) if !status.phone_verified => rsx! {
            CallerIdVerification { revision, session_state }
        },
        Some(Ok(status)) if status.needs_number_claim => rsx! {
            NumberClaim { revision, session_state }
        },
        Some(Ok(_)) => rsx! {
            AppShell {
                current,
                session_state,
                startup_pending: false,
                launch_chat_initialized,
            }
        },
    }
}

#[component]
fn CallerIdVerification(
    mut revision: Signal<u32>,
    mut session_state: Signal<Option<Session>>,
    #[props(default)] on_cancel: Option<EventHandler<()>>,
) -> Element {
    let mut phone_number = use_signal(String::new);
    let mut validation_code = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut message = use_signal(|| {
        "Verify the mobile number people should see when Courtside calls.".to_string()
    });

    let mut start = move || {
        if busy() {
            return;
        }
        let phone = normalize_caller_id_input(&phone_number());
        if !phone.starts_with('+') {
            message.set("Enter a valid US mobile number.".into());
            return;
        }
        phone_number.set(phone.clone());
        busy.set(true);
        message.set("Preparing your verification call…".into());
        spawn(async move {
            match api::start_phone_verification(phone).await {
                Ok(response) => {
                    validation_code.set(response.validation_code);
                    message.set("Twilio will call in a few seconds. Answer it and enter the code shown below.".into());
                }
                Err(error) => message.set(error),
            }
            busy.set(false);
        });
    };

    let mut confirm = move || {
        if busy() {
            return;
        }
        busy.set(true);
        message.set("Checking caller-ID verification…".into());
        let phone = phone_number();
        let code = validation_code();
        spawn(async move {
            match api::confirm_phone_verification(phone, code).await {
                Ok(_) => *revision.write() += 1,
                Err(error) => {
                    message.set(error);
                    busy.set(false);
                }
            }
        });
    };

    rsx! {
        main { class: "setup-page number-page",
            div { class: "number-shell verification-shell",
                div { class: "brand-lockup compact",
                    Logo { size: 46 }
                    div { h2 { "Courtside" } p { "Your phone assistant" } }
                }
                span { class: "eyebrow", "CALLER ID" }
                h1 { "Show your number" }
                p { class: "form-intro",
                    "Courtside will call from your verified mobile number, so recipients recognize you."
                }
                if validation_code().is_empty() {
                    form {
                        class: "number-search",
                        onsubmit: move |event| {
                            event.prevent_default();
                            start();
                        },
                        div { class: "field-with-icon",
                            Icon { kind: IconKind::Phone, size: 21 }
                            input {
                                r#type: "tel",
                                inputmode: "tel",
                                autocomplete: "tel",
                                enterkeyhint: "go",
                                placeholder: "+1 530 555 0123",
                                value: "{phone_number}",
                                oninput: move |event| phone_number.set(event.value()),
                            }
                        }
                        button { class: "primary-button", r#type: "submit", disabled: busy(),
                            if busy() { "Starting call…" } else { "Verify my number" }
                            Icon { kind: IconKind::ArrowRight, size: 19 }
                        }
                    }
                } else {
                    div { class: "verification-card",
                        span { class: "verification-label", "ENTER DURING THE TWILIO CALL" }
                        strong { class: "verification-code", "{validation_code}" }
                        div { class: "verification-steps",
                            p { "1. Answer the incoming verification call." }
                            p { "2. Enter this code on your phone keypad." }
                            p { "3. Return here and check verification." }
                        }
                        button {
                            class: "primary-button",
                            disabled: busy(),
                            onclick: move |_| confirm(),
                            if busy() { "Checking…" } else { "I entered it — check" }
                        }
                        button {
                            class: "text-button",
                            disabled: busy(),
                            onclick: move |_| {
                                validation_code.set(String::new());
                                message.set("You can request another verification call after one minute.".into());
                            },
                            "Use a different number"
                        }
                    }
                }
                p { class: "form-message", "{message}" }
                p { class: "verification-note",
                    "Only a number you prove you own can be used. Courtside never allows arbitrary caller IDs."
                }
                if let Some(on_cancel) = on_cancel {
                    button { class: "text-button", onclick: move |_| on_cancel.call(()), "Back to account" }
                }
                button {
                    class: "text-button danger",
                    onclick: move |_| sign_out(&mut session_state),
                    "Sign out"
                }
            }
        }
    }
}

#[component]
fn SetupMessage(
    title: &'static str,
    body: String,
    primary: &'static str,
    on_primary: EventHandler<()>,
    mut session_state: Signal<Option<Session>>,
) -> Element {
    rsx! {
        main { class: "setup-page",
            div { class: "setup-card",
                Logo { size: 64 }
                span { class: "eyebrow", "ACCOUNT SETUP" }
                h1 { "{title}" }
                p { "{body}" }
                button { class: "primary-button", onclick: move |_| on_primary.call(()), "{primary}" }
                button {
                    class: "text-button danger",
                    onclick: move |_| sign_out(&mut session_state),
                    "Sign out"
                }
            }
        }
    }
}

#[component]
fn NumberClaim(
    mut revision: Signal<u32>,
    mut session_state: Signal<Option<Session>>,
    #[props(default)] on_cancel: Option<EventHandler<()>>,
    #[props(default = "Back to account")] cancel_label: &'static str,
) -> Element {
    let mut area_code = use_signal(String::new);
    let mut results = use_signal(Vec::<AvailableNumber>::new);
    let mut message =
        use_signal(|| "Choose the number people will see when Courtside calls.".to_string());
    let mut busy = use_signal(|| false);

    let mut search = move || {
        if busy() {
            return;
        }
        let code = area_code().trim().to_string();
        if !code.is_empty() && (code.len() != 3 || !code.chars().all(|c| c.is_ascii_digit())) {
            message.set("Enter a three-digit US area code.".into());
            return;
        }
        busy.set(true);
        message.set("Finding available numbers…".into());
        spawn(async move {
            match api::search_numbers(code).await {
                Ok(response) if response.numbers.is_empty() => {
                    results.set(Vec::new());
                    message.set("No numbers found. Try a nearby area code.".into());
                }
                Ok(response) => {
                    results.set(response.numbers);
                    message.set("Pick a number to finish setup.".into());
                }
                Err(error) => message.set(error),
            }
            busy.set(false);
        });
    };

    rsx! {
        main { class: "setup-page number-page",
            div { class: "number-shell",
                div { class: "brand-lockup compact",
                    Logo { size: 46 }
                    div { h2 { "Courtside" } p { "Your phone assistant" } }
                }
                span { class: "eyebrow", if cfg!(target_arch = "wasm32") { "YOUR NUMBER" } else { "FINAL STEP" } }
                h1 { "Claim your number" }
                p { class: "form-intro", "Get a dedicated Courtside number for your AI to answer incoming and forwarded calls." }
                form {
                    class: "number-search",
                    onsubmit: move |event| {
                        event.prevent_default();
                        search();
                    },
                    div { class: "field-with-icon",
                        Icon { kind: IconKind::Location, size: 21 }
                        input {
                            r#type: "text",
                            inputmode: "numeric",
                            maxlength: "3",
                            enterkeyhint: "search",
                            placeholder: "Area code, e.g. 415",
                            value: "{area_code}",
                            oninput: move |event| area_code.set(event.value()),
                        }
                    }
                    button { class: "primary-button", r#type: "submit", disabled: busy(),
                        Icon { kind: IconKind::Search, size: 19 }
                        if busy() { "Searching…" } else { "Search numbers" }
                    }
                }
                p { class: "form-message", "{message}" }
                if let Some(on_cancel) = on_cancel {
                    button { class: "text-button", onclick: move |_| on_cancel.call(()), "{cancel_label}" }
                }
                div { class: "number-results",
                    for number in results() {
                        {
                            let phone = number.phone_number.clone();
                            let locality = number.locality.clone().unwrap_or_default();
                            let region = number.region.clone().unwrap_or_default();
                            let place = match (locality.is_empty(), region.is_empty()) {
                                (false, false) => format!("{locality}, {region}"),
                                (false, true) => locality,
                                (true, false) => region,
                                (true, true) => "United States".into(),
                            };
                            rsx! {
                                div { class: "number-row", key: "{phone}",
                                    div {
                                        strong { "{number.friendly_name}" }
                                        span { "{place}" }
                                    }
                                    button {
                                        disabled: busy(),
                                        onclick: move |_| {
                                            let selected = phone.clone();
                                            busy.set(true);
                                            message.set("Claiming your number…".into());
                                            spawn(async move {
                                                match api::purchase_number(selected).await {
                                                    Ok(_) => *revision.write() += 1,
                                                    Err(error) => {
                                                        message.set(error);
                                                        busy.set(false);
                                                    }
                                                }
                                            });
                                        },
                                        "Claim"
                                    }
                                }
                            }
                        }
                    }
                }
                button {
                    class: "text-button danger",
                    onclick: move |_| sign_out(&mut session_state),
                    "Sign out"
                }
            }
        }
    }
}

#[derive(Clone, PartialEq)]
struct CallQuestionNotice {
    call_id: Uuid,
    request: CallQuestion,
}

fn recover_call_question(
    detail: &CallDetailResponse,
    mut question: Signal<Option<CallQuestionNotice>>,
) {
    if let Some(request) = detail.pending_question.as_ref().filter(|_| {
        detail.call.status == CallStatus::InProgress && detail.mode == CallMode::Assistant
    }) {
        question.set(Some(CallQuestionNotice {
            call_id: detail.call.id,
            request: request.clone(),
        }));
    } else if question
        .peek()
        .as_ref()
        .is_some_and(|notice| notice.call_id == detail.call.id)
    {
        question.set(None);
    }
}

#[derive(Clone, PartialEq)]
struct IncomingCallNotice {
    call: Call,
    chat_id: Option<Uuid>,
    handoff: Option<CallHandoffRequest>,
    update: String,
    mode: CallMode,
}

fn recover_incoming_call(
    detail: &CallDetailResponse,
    mut incoming: Signal<Option<IncomingCallNotice>>,
    mut invited: Signal<Option<Uuid>>,
) {
    if detail.call.direction != shared::CallDirection::Inbound {
        return;
    }
    if matches!(
        detail.call.status,
        CallStatus::Completed | CallStatus::Failed
    ) {
        if incoming
            .peek()
            .as_ref()
            .is_some_and(|notice| notice.call.id == detail.call.id)
        {
            incoming.set(None);
        }
        return;
    }
    let previous = incoming
        .peek()
        .clone()
        .filter(|notice| notice.call.id == detail.call.id);
    let had_handoff = previous
        .as_ref()
        .is_some_and(|notice| notice.handoff.is_some());
    let handoff = detail
        .handoff
        .clone()
        .filter(|request| detail.mode == CallMode::Assistant && request.expires_at > Utc::now());
    incoming.set(Some(IncomingCallNotice {
        call: detail.call.clone(),
        chat_id: detail.chat_id,
        handoff: handoff.clone(),
        update: previous.map(|notice| notice.update).unwrap_or_default(),
        mode: detail.mode,
    }));
    if let Some(request) = handoff {
        if *invited.peek() != Some(request.id) {
            // Mark first so native callbacks or duplicate snapshots cannot
            // present a second CallKit invitation for the same request.
            invited.set(Some(request.id));
            let peer = if request.caller_name.is_empty() {
                &detail.call.peer_number
            } else {
                &request.caller_name
            };
            let seconds = (request.expires_at - Utc::now()).num_milliseconds() as f64 / 1000.0;
            if let Err(error) = voice::invite(detail.call.id, peer, seconds) {
                incoming.with_mut(|notice| {
                    if let Some(notice) = notice {
                        notice.update = error;
                    }
                });
            }
        }
    } else if had_handoff {
        // A caller hangup/timeout cancels a ringing invitation. A handoff
        // resolved by actual acceptance must keep the connected audio alive.
        voice::cancel_invitation(detail.call.id);
    }
}

#[component]
fn AppShell(
    current: Session,
    mut session_state: Signal<Option<Session>>,
    startup_pending: bool,
    mut launch_chat_initialized: Signal<bool>,
) -> Element {
    let mut pane = use_signal(|| Pane::Assistant);
    let mut drawer_open = use_signal(|| false);
    let mut calls = use_signal(Vec::<Call>::new);
    let mut calls_error = use_signal(String::new);
    let mut calls_revision = use_signal(|| 0_u32);
    let mut chat_revision = use_signal(|| 0_u32);
    let mut chats_revision = use_signal(|| 0_u32);
    let chat_sending = use_signal(|| false);
    let mut chat_turn = use_signal(|| None::<ChatTurnActivity>);
    use_context_provider(|| chat_turn);
    let mut recent_chats = use_signal(Vec::<ChatSummary>::new);
    let mut current_chat_id = use_signal(|| None::<Uuid>);
    let mut live_chat_message = use_signal(|| None::<ChatMessage>);
    let mut live_call = use_signal(|| None::<LiveCallTranscript>);
    let mut chat_call_id = use_signal(|| None::<Uuid>);
    let mut incoming = use_signal(|| None::<IncomingCallNotice>);
    let mut call_question = use_signal(|| None::<CallQuestionNotice>);
    use_context_provider(|| call_question);
    let invited = use_signal(|| None::<Uuid>);
    let mut incoming_busy = use_signal(|| false);
    let mut incoming_error = use_signal(String::new);
    let mut voice_status = use_signal(|| None::<voice::Event>);
    use_context_provider(|| voice_status);
    let mut listen_status = use_signal(|| None::<listen::Event>);
    use_context_provider(|| listen_status);
    use_drop(|| {
        api::stop_events();
        listen::stop(None);
        voice::end(None);
    });
    use_future(move || async move {
        let events = listen::subscribe();
        while let Ok(event) = events.recv().await {
            listen_status.set(Some(event));
        }
    });
    use_future(move || async move {
        let events = voice::subscribe();
        while let Ok(event) = events.recv().await {
            if matches!(event.phase, voice::Phase::Declined | voice::Phase::Error) {
                let request = incoming
                    .peek()
                    .as_ref()
                    .filter(|notice| notice.call.id == event.call_id)
                    .and_then(|notice| notice.handoff.as_ref().map(|request| request.id));
                if let Some(request_id) = request {
                    spawn(async move {
                        let _ = api::decline_call_handoff(event.call_id, request_id).await;
                    });
                }
            }
            live_call.with_mut(|current| {
                if let Some(feed) = current
                    .as_mut()
                    .filter(|feed| feed.call_id == event.call_id)
                {
                    feed.control_message = event.message.clone();
                    if event.phase == voice::Phase::Connected {
                        feed.mode = CallMode::InAppTakeover;
                    }
                }
            });
            voice_status.set(Some(event));
        }
    });

    let _calls_loader = use_resource(move || {
        let _ = calls_revision();
        async move {
            match api::list_calls().await {
                Ok(response) => {
                    let active_call = response
                        .calls
                        .iter()
                        .find(|call| {
                            matches!(
                                call.status,
                                CallStatus::Queued | CallStatus::Ringing | CallStatus::InProgress
                            )
                        })
                        .map(|call| call.id);
                    calls.set(response.calls);
                    calls_error.set(String::new());
                    if let Some(id) = active_call {
                        if let Ok(detail) = api::call_detail(id).await {
                            recover_incoming_call(&detail, incoming, invited);
                            recover_call_question(&detail, call_question);
                        }
                    }
                }
                Err(error) => calls_error.set(error),
            }
        }
    });

    let incoming_id = use_memo(move || incoming().map(|notice| notice.call.id));
    let _incoming_snapshot = use_resource(move || {
        let watched = incoming_id();
        async move {
            let Some(id) = watched else { return };
            let Ok(updates) = api::watch_call(id) else {
                return;
            };
            while let Ok(detail) = updates.recv().await {
                recover_incoming_call(&detail, incoming, invited);
                recover_call_question(&detail, call_question);
                if matches!(
                    detail.call.status,
                    CallStatus::Completed | CallStatus::Failed
                ) {
                    voice::end(Some(id));
                    break;
                }
            }
        }
    });

    let _chats_loader = use_resource(move || {
        let _ = chats_revision();
        async move {
            if let Ok(response) = api::recent_chats().await {
                recent_chats.set(response.chats);
            }
        }
    });

    // A call can connect before send_chat returns, or while iOS suspends the
    // event socket. Recover its actual state instead of leaving controls disabled.
    let _call_snapshot = use_resource(move || {
        let watched = chat_call_id();
        async move {
            let Some(call_id) = watched else {
                return;
            };
            let Ok(updates) = api::watch_call(call_id) else {
                return;
            };
            while let Ok(detail) = updates.recv().await {
                recover_call_question(&detail, call_question);
                if matches!(
                    detail.call.status,
                    CallStatus::Completed | CallStatus::Failed
                ) {
                    voice::end(Some(call_id));
                    listen::stop(Some(call_id));
                }
                calls.with_mut(|items| {
                    if let Some(call) = items.iter_mut().find(|call| call.id == call_id) {
                        *call = detail.call.clone();
                    } else {
                        items.insert(0, detail.call.clone());
                    }
                });
                live_call.with_mut(|current| {
                    if let Some(feed) = current.as_mut().filter(|feed| feed.call_id == call_id) {
                        feed.refresh(detail);
                    }
                });
            }
        }
    });

    // Keep the app in sync while a call is running. Completed summaries arrive
    // in chat on their own, and the Calls screen updates without a manual reload.
    let _live_events = use_future(move || async move {
        let Ok(events) = api::subscribe_events() else {
            return;
        };
        while let Ok(event) = events.recv().await {
            match event {
                ServerEvent::ChatActivity {
                    chat_id,
                    request_id,
                    activity,
                } => {
                    chat_turn.with_mut(|pending| {
                        if let Some(turn) = pending
                            .as_mut()
                            .filter(|turn| turn.chat_id == chat_id && turn.request_id == request_id)
                        {
                            turn.activity = activity;
                        }
                    });
                }
                ServerEvent::IncomingCall { call, chat_id } => {
                    incoming_error.set(String::new());
                    incoming.set(Some(IncomingCallNotice {
                        call: call.clone(),
                        chat_id: Some(chat_id),
                        handoff: None,
                        update: "Courtside is answering and learning why they're calling.".into(),
                        mode: CallMode::Assistant,
                    }));
                    calls.with_mut(|items| {
                        if !items.iter().any(|item| item.id == call.id) {
                            items.insert(0, call);
                        }
                    });
                    *chats_revision.write() += 1;
                }
                ServerEvent::CallHandoffChanged { call_id, request } => {
                    let was_pending = incoming.peek().as_ref().is_some_and(|notice| {
                        notice.call.id == call_id && notice.handoff.is_some()
                    });
                    if request.is_none() && was_pending {
                        voice::cancel_invitation(call_id);
                    }
                    incoming.with_mut(|current| {
                        if let Some(notice) =
                            current.as_mut().filter(|notice| notice.call.id == call_id)
                        {
                            notice.handoff = request;
                        }
                    });
                    spawn(async move {
                        if let Ok(detail) = api::call_detail(call_id).await {
                            recover_incoming_call(&detail, incoming, invited);
                        }
                    });
                }
                ServerEvent::CallStatusChanged { call_id, status } => {
                    if matches!(status, CallStatus::Completed | CallStatus::Failed) {
                        voice::end(Some(call_id));
                        listen::stop(Some(call_id));
                        if call_question
                            .peek()
                            .as_ref()
                            .is_some_and(|notice| notice.call_id == call_id)
                        {
                            call_question.set(None);
                        }
                        if incoming
                            .peek()
                            .as_ref()
                            .is_some_and(|notice| notice.call.id == call_id)
                        {
                            incoming.set(None);
                        }
                    }
                    let mut found = false;
                    calls.with_mut(|items| {
                        if let Some(call) = items.iter_mut().find(|call| call.id == call_id) {
                            call.status = status;
                            found = true;
                        }
                    });
                    if !found {
                        *calls_revision.write() += 1;
                    }
                    if chat_call_id() == Some(call_id) {
                        live_call.with_mut(|current| {
                            if matches!(
                                status,
                                CallStatus::Queued | CallStatus::Ringing | CallStatus::InProgress
                            ) {
                                match current {
                                    Some(feed) if feed.call_id == call_id => feed.status = status,
                                    _ => *current = Some(LiveCallTranscript::new(call_id, status)),
                                }
                            } else if let Some(feed) = current {
                                if feed.call_id == call_id {
                                    feed.status = status;
                                }
                            }
                        });
                    }
                    if chat_call_id() == Some(call_id)
                        && matches!(status, CallStatus::Completed | CallStatus::Failed)
                    {
                        spawn(async move {
                            if let Ok(detail) = api::call_detail(call_id).await {
                                live_call.set(Some(LiveCallTranscript::from_detail(detail)));
                            }
                        });
                    }
                }
                ServerEvent::SummaryReady { call_id, summary } => {
                    let mut found = false;
                    calls.with_mut(|items| {
                        if let Some(call) = items.iter_mut().find(|call| call.id == call_id) {
                            call.summary = Some(summary);
                            found = true;
                        }
                    });
                    if !found {
                        *calls_revision.write() += 1;
                    }
                    if chat_call_id() == Some(call_id) {
                        spawn(async move {
                            if let Ok(detail) = api::call_detail(call_id).await {
                                live_call.set(Some(LiveCallTranscript::from_detail(detail)));
                            }
                        });
                    }
                }
                ServerEvent::CallRecallChanged {
                    call_id,
                    remembering,
                } => {
                    live_call.with_mut(|current| {
                        if let Some(feed) = current.as_mut().filter(|feed| feed.call_id == call_id)
                        {
                            feed.remembering = remembering;
                        }
                    });
                }
                ServerEvent::CallQuestionChanged { call_id, question } => {
                    if let Some(request) = question.clone() {
                        call_question.set(Some(CallQuestionNotice { call_id, request }));
                    } else if call_question
                        .peek()
                        .as_ref()
                        .is_some_and(|notice| notice.call_id == call_id)
                    {
                        call_question.set(None);
                    }
                    live_call.with_mut(|current| {
                        if let Some(feed) = current.as_mut().filter(|feed| feed.call_id == call_id)
                        {
                            feed.pending_question = question;
                        }
                    });
                }
                ServerEvent::ChatMessageAdded { call_id, message } => {
                    *chats_revision.write() += 1;
                    incoming.with_mut(|current| {
                        if let Some(notice) =
                            current.as_mut().filter(|notice| notice.call.id == call_id)
                        {
                            notice.update = message.content.clone();
                        }
                    });
                    if chat_call_id() == Some(call_id) {
                        live_chat_message.set(Some(message));
                    }
                }
                ServerEvent::CallModeChanged {
                    call_id,
                    mode,
                    message,
                } => {
                    incoming.with_mut(|current| {
                        if let Some(notice) =
                            current.as_mut().filter(|notice| notice.call.id == call_id)
                        {
                            notice.mode = mode;
                        }
                    });
                    if chat_call_id() == Some(call_id) {
                        live_call.with_mut(|current| {
                            if let Some(feed) =
                                current.as_mut().filter(|feed| feed.call_id == call_id)
                            {
                                feed.mode = mode;
                                feed.control_message = message;
                            }
                        });
                    }
                }
                ServerEvent::TranscriptDelta {
                    call_id,
                    speaker,
                    delta,
                    start_ms,
                    end_ms,
                } => {
                    if chat_call_id() == Some(call_id) {
                        live_call.with_mut(|current| {
                            let feed = match current {
                                Some(feed) if feed.call_id == call_id => feed,
                                _ => current.insert(LiveCallTranscript::new(
                                    call_id,
                                    CallStatus::InProgress,
                                )),
                            };
                            if matches!(feed.status, CallStatus::Queued | CallStatus::Ringing) {
                                feed.status = CallStatus::InProgress;
                            }
                            feed.push_delta(speaker, delta, start_ms, end_ms);
                        });
                    }
                }
                ServerEvent::Transcript { .. } => {}
            }
        }
    });

    rsx! {
        main { class: "app-shell",
            match pane() {
                Pane::Assistant => rsx! {
                    ChatScreen { calls, chat_revision, chats_revision, current_chat_id, sending: chat_sending, drawer_open, live_chat_message, live_call, chat_call_id, launch_chat_initialized }
                },
                Pane::Calls => rsx! {
                    CallsScreen { calls, calls_error, calls_revision, pane, drawer_open }
                },
                Pane::Forwarding => rsx! {
                    ForwardingScreen { pane, drawer_open, session_state }
                },
                Pane::Account => rsx! {
                    AccountScreen { user: current.user.clone(), session_state, pane, drawer_open }
                },
            }

            if let Some(notice) = call_question().filter(|notice| pane() != Pane::Assistant || current_chat_id() != Some(notice.request.chat_id)) {
                aside { class: "incoming-call-banner call-question-banner",
                    CallQuestionPrompt { key: "{notice.request.id}", call_id: notice.call_id, request: notice.request.clone() }
                    button { class: "question-open-chat",
                        disabled: incoming_busy() || chat_sending(),
                        onclick: move |_| {
                            incoming_busy.set(true);
                            incoming_error.set(String::new());
                            spawn(async move {
                                match api::select_chat(notice.request.chat_id).await {
                                    Ok(response) => {
                                        launch_chat_initialized.set(true);
                                        current_chat_id.set(Some(response.chat_id));
                                        live_chat_message.set(None);
                                        if let Some(detail) = response.call {
                                            chat_call_id.set(Some(detail.call.id));
                                            live_call.set(Some(LiveCallTranscript::from_detail(detail)));
                                        }
                                        *chat_revision.write() += 1;
                                        pane.set(Pane::Assistant);
                                        drawer_open.set(false);
                                    }
                                    Err(error) => incoming_error.set(error),
                                }
                                incoming_busy.set(false);
                            });
                        },
                        "Answer in chat"
                    }
                    if !incoming_error().is_empty() { p { "{incoming_error}" } }
                }
            }
            if let Some(notice) = incoming().filter(|notice|
                call_question().is_none() && (pane() != Pane::Assistant || chat_call_id() != Some(notice.call.id))) {
                aside { class: "incoming-call-banner", aria_live: "polite",
                    div { class: "incoming-call-heading",
                        span { class: "live-call-dot" }
                        strong {
                            if notice.mode == CallMode::InAppTakeover { "You're on the incoming call" }
                            else if notice.handoff.is_some() { "Caller wants to speak with you" }
                            else { "Courtside is screening a call" }
                        }
                        span { "{notice.call.peer_number}" }
                    }
                    if let Some(request) = notice.handoff.as_ref() {
                        p { "{request.caller_name} · {request.reason}" }
                    } else {
                        p { "{notice.update}" }
                    }
                    div { class: "incoming-call-actions",
                        button {
                            disabled: incoming_busy() || chat_sending(),
                            onclick: move |_| {
                                let Some(chat_id) = notice.chat_id else { return };
                                incoming_busy.set(true);
                                incoming_error.set(String::new());
                                spawn(async move {
                                    match api::select_chat(chat_id).await {
                                        Ok(response) => {
                                            launch_chat_initialized.set(true);
                                            current_chat_id.set(Some(response.chat_id));
                                            live_chat_message.set(None);
                                            if let Some(detail) = response.call {
                                                chat_call_id.set(Some(detail.call.id));
                                                live_call.set(Some(LiveCallTranscript::from_detail(detail)));
                                            }
                                            *chat_revision.write() += 1;
                                            pane.set(Pane::Assistant);
                                            drawer_open.set(false);
                                        }
                                        Err(error) => incoming_error.set(error),
                                    }
                                    incoming_busy.set(false);
                                });
                            },
                            "View call"
                        }
                        if let Some(request) = notice.handoff {
                            button { class: "incoming-join",
                                disabled: incoming_busy() || voice_status().is_some_and(|event| {
                                    event.call_id == notice.call.id && matches!(event.phase, voice::Phase::Connecting | voice::Phase::Connected)
                                }),
                                onclick: move |_| {
                                    incoming_error.set(String::new());
                                    if let Err(error) = voice::start(notice.call.id, &notice.call.peer_number) {
                                        incoming_error.set(error);
                                    }
                                },
                                "Join"
                            }
                            button {
                                disabled: incoming_busy(),
                                onclick: move |_| {
                                    incoming_busy.set(true);
                                    incoming_error.set(String::new());
                                    spawn(async move {
                                        if let Err(error) = api::decline_call_handoff(notice.call.id, request.id).await {
                                            incoming_error.set(error);
                                        }
                                        incoming_busy.set(false);
                                    });
                                },
                                "Take a message"
                            }
                        }
                    }
                    if !incoming_error().is_empty() { p { class: "error", "{incoming_error}" } }
                }
            }

            Drawer {
                user: current.user.clone(),
                recent_chats,
                current_chat_id,
                pane,
                drawer_open,
                chat_revision,
                chats_revision,
                chat_sending,
                live_chat_message,
                live_call,
                chat_call_id,
            }
            if startup_pending {
                div {
                    class: "startup-input-guard",
                    aria_label: "Opening Courtside",
                }
            }
        }
    }
}

#[component]
fn ScreenHeader(title: &'static str, subtitle: &'static str, drawer_open: Signal<bool>) -> Element {
    rsx! {
        header { class: "screen-header",
            div { h1 { "{title}" } p { "{subtitle}" } }
            MenuButton { drawer_open }
        }
    }
}

#[component]
fn MenuButton(mut drawer_open: Signal<bool>) -> Element {
    rsx! {
        button {
            class: "circle-button menu-button",
            aria_label: "Open navigation",
            onclick: move |_| drawer_open.set(true),
            Icon { kind: IconKind::Menu, size: 23 }
        }
    }
}

#[component]
fn ChatScreen(
    mut calls: Signal<Vec<Call>>,
    chat_revision: Signal<u32>,
    mut chats_revision: Signal<u32>,
    mut current_chat_id: Signal<Option<Uuid>>,
    mut sending: Signal<bool>,
    drawer_open: Signal<bool>,
    live_chat_message: Signal<Option<ChatMessage>>,
    mut live_call: Signal<Option<LiveCallTranscript>>,
    mut chat_call_id: Signal<Option<Uuid>>,
    mut launch_chat_initialized: Signal<bool>,
) -> Element {
    let mut messages = use_signal(Vec::<ChatMessage>::new);
    let mut draft = use_signal(String::new);
    let mut send_error = use_signal(String::new);
    let mut chat_turn = use_context::<Signal<Option<ChatTurnActivity>>>();
    let mut contact_picker_open = use_signal(|| false);
    let mut contact_matches = use_signal(Vec::<crate::contacts::ContactSelection>::new);
    let mut contacts_loading = use_signal(|| false);
    let mut contacts_error = use_signal(String::new);
    let mut contacts_request = use_signal(|| 0u64);
    let mut chat_mounted = use_signal(|| false);
    let menu_owner = use_hook(crate::native::composer_menu_owner);

    use_future(move || async move {
        if !cfg!(target_os = "ios") {
            return;
        }
        let script =
            include_str!("composer-menus.js").replace("__OWNER__", &menu_owner.to_string());
        let mut tracker = document::eval(&script);
        while let Ok(frames) = tracker
            .recv::<Option<crate::native::ComposerMenuFrames>>()
            .await
        {
            crate::native::set_composer_menu_frames(menu_owner, frames);
            if frames.is_none() {
                break;
            }
        }
    });
    use_drop(move || {
        crate::native::set_composer_menu_frames(menu_owner, None);
        if cfg!(target_os = "ios") {
            let _ = document::eval(&format!(
                "window.courtsideComposerMenus?.get({menu_owner})?.();"
            ));
        }
    });

    let contact_query = use_memo(move || trailing_contact_query(&draft()));
    use_effect(move || {
        let query = contact_query();
        let request = contacts_request.peek().wrapping_add(1);
        contacts_request.set(request);
        contact_matches.set(Vec::new());
        contacts_error.set(String::new());
        let Some(query) = query else {
            contacts_loading.set(false);
            return;
        };
        if query.trim().is_empty() {
            contacts_loading.set(false);
            return;
        }
        contacts_loading.set(true);
        spawn(async move {
            let result = crate::contacts::search_contacts(query).await;
            if contacts_request() != request {
                return;
            }
            match result {
                Ok(matches) => contact_matches.set(matches),
                Err(error) => contacts_error.set(error),
            }
            contacts_loading.set(false);
        });
    });

    // Suggestions, contact insertion, sending, and retrying can update the
    // draft without a DOM input event. Measure after Dioxus commits the value.
    use_effect(move || {
        let _draft = draft();
        spawn(async move {
            let _ = document::eval("window.courtsideLayout?.schedule();").await;
        });
    });

    let _history = use_resource(move || {
        let _ = chat_revision();
        let should_start_fresh = !*launch_chat_initialized.peek();
        if should_start_fresh {
            launch_chat_initialized.set(true);
        }
        messages.set(Vec::new());
        draft.set(String::new());
        send_error.set(String::new());
        async move {
            let response = if should_start_fresh {
                api::new_chat().await
            } else {
                api::chat_history().await
            };
            match response {
                Ok(response) => {
                    current_chat_id.set(Some(response.chat_id));
                    messages.set(response.messages);
                    match response.call {
                        Some(detail) => {
                            chat_call_id.set(Some(detail.call.id));
                            live_call.set(Some(LiveCallTranscript::from_detail(detail)));
                        }
                        None => {
                            chat_call_id.set(None);
                            live_call.set(None);
                        }
                    }
                    if should_start_fresh {
                        *chats_revision.write() += 1;
                    }
                }
                Err(error) => send_error.set(format!("Couldn't start a fresh chat: {error}")),
            }
        }
    });

    use_effect(move || {
        let Some(update) = live_chat_message() else {
            return;
        };
        messages.with_mut(|current| {
            if !current.iter().any(|message| message.id == update.id) {
                current.push(update);
            }
        });
    });

    let mut send = move || {
        if sending() {
            return;
        }
        let content = draft().trim().to_string();
        if content.is_empty() {
            return;
        }
        if current_chat_id().is_none() {
            send_error.set("Your fresh chat is still opening. Try again in a moment.".into());
            return;
        }
        send_error.set(String::new());
        draft.set(String::new());
        sending.set(true);
        let pending_id = Uuid::new_v4();
        let sent_chat_id = current_chat_id().expect("checked above");
        chat_turn.set(Some(ChatTurnActivity {
            chat_id: sent_chat_id,
            request_id: pending_id,
            activity: None,
        }));
        messages.write().push(ChatMessage {
            id: pending_id,
            role: ChatRole::User,
            content: content.clone(),
            at: Utc::now(),
        });
        let retry_content = content.clone();
        spawn(async move {
            let response = api::send_chat(content, pending_id).await;
            if chat_turn().is_some_and(|turn| turn.request_id == pending_id) {
                chat_turn.set(None);
            }
            match response {
                Ok(response) => {
                    *chats_revision.write() += 1;
                    if current_chat_id() != Some(sent_chat_id) {
                        sending.set(false);
                        return;
                    }
                    if let Some(started_call) = response.started_call {
                        chat_call_id.set(Some(started_call.id));
                        live_call.set(Some(LiveCallTranscript::new(
                            started_call.id,
                            started_call.status,
                        )));
                        calls.write().insert(0, started_call);
                    }
                    messages.with_mut(|messages| {
                        if let Some(pending) =
                            messages.iter_mut().find(|message| message.id == pending_id)
                        {
                            *pending = response.user_message;
                        } else {
                            messages.push(response.user_message);
                        }
                        messages.push(response.reply);
                    });
                }
                Err(error) => {
                    if current_chat_id() == Some(sent_chat_id) {
                        messages.write().retain(|message| message.id != pending_id);
                        draft.set(retry_content);
                        send_error.set(error);
                    }
                }
            }
            sending.set(false);
        });
    };

    let mut choose_contact = move || {
        if sending() || contact_picker_open() {
            return;
        }
        contact_picker_open.set(true);
        send_error.set(String::new());
        spawn(async move {
            match crate::contacts::pick_contact().await {
                Ok(Some(contact)) => {
                    draft.set(draft_with_contact(&draft(), &contact));
                }
                Ok(None) => {}
                Err(error) => send_error.set(error),
            }
            contact_picker_open.set(false);
        });
    };

    let inline_contact_query = if cfg!(target_arch = "wasm32") {
        None
    } else {
        trailing_contact_query(&draft())
    };

    let current_messages = messages();
    let current_live_call = live_call();
    let pending_answer = current_live_call
        .as_ref()
        .filter(|feed| feed.status == CallStatus::InProgress && feed.mode == CallMode::Assistant)
        .and_then(|feed| {
            feed.pending_question
                .clone()
                .map(|request| (feed.call_id, request, feed.remembering))
        });
    let call_record = current_live_call
        .as_ref()
        .and_then(|feed| calls().into_iter().find(|call| call.id == feed.call_id));
    let live_peer_number = call_record
        .as_ref()
        .map(|call| call.peer_number.clone())
        .unwrap_or_else(|| "Active call".into());
    let chat_is_empty = current_messages.is_empty() && current_live_call.is_none();
    // Anchor the transcript immediately after the assistant message that
    // started its call. Later chat replies then flow beneath it naturally
    // instead of forcing the transcript to remain the last item forever.
    let call_insert_index = call_record.as_ref().and_then(|call| {
        current_messages.iter().position(|message| {
            message.role == ChatRole::Assistant && message.at >= call.created_at
        })
    });
    let mut call_to_place = current_live_call.clone();
    let timeline_messages = current_messages
        .into_iter()
        .enumerate()
        .map(|(index, message)| {
            let call_after = if Some(index) == call_insert_index {
                call_to_place.take()
            } else {
                None
            };
            (message, call_after)
        })
        .collect::<Vec<_>>();
    let trailing_call = call_to_place;
    use_effect(move || {
        let _message_count = messages().len();
        let _is_sending = sending();
        let _live_update = live_call()
            .and_then(|feed| feed.lines.last().map(|line| (line.end_ms, line.text.len())));
        if chat_mounted() {
            spawn(async move {
                let _ = document::eval(
                    "window.courtsideLayout?.scrollToEnd('courtside-chat-content', 'smooth');",
                )
                .await;
            });
        }
    });
    rsx! {
        section { class: if chat_is_empty { "screen chat-screen empty-chat" } else { "screen chat-screen" },
            header { class: "assistant-header",
                MenuButton { drawer_open }
                div { class: "brand-lockup compact chat-brand",
                    div { h2 { "Courtside" } p { "Your assistant" } }
                    Logo { size: 40 }
                }
            }
            if chat_is_empty {
                div { class: "suggestion-row top-suggestions",
                    button {
                        disabled: sending(),
                        onclick: move |_| draft.set("Make a restaurant reservation for me".into()),
                        "Restaurant"
                    }
                    button {
                        disabled: sending(),
                        onclick: move |_| draft.set("Help me schedule a clinic appointment".into()),
                        "Clinic"
                    }
                    button {
                        disabled: sending(),
                        onclick: move |_| draft.set("Follow up on an earlier call".into()),
                        "Follow up"
                    }
                }
            }
            div {
                id: "courtside-chat-content",
                class: if chat_is_empty { "chat-content empty" } else { "chat-content" },
                if chat_is_empty {
                    div { class: "assistant-hero",
                        LiquidAquarium {}
                        h1 { "What should I handle?" }
                        p { "Plan a call or ask about one." }
                    }
                } else {
                    div { class: "message-list",
                        for (message, call_after) in timeline_messages {
                            div {
                                key: "{message.id}",
                                class: if message.role == ChatRole::User { "message user-message" } else { "message assistant-message" },
                                "{message.content}"
                            }
                            if let Some(feed) = call_after {
                                LiveCallCard { key: "{feed.call_id}", feed, peer_number: live_peer_number.clone(), live_call }
                            }
                        }
                        if let Some(feed) = trailing_call {
                            LiveCallCard { key: "{feed.call_id}", feed, peer_number: live_peer_number, live_call }
                        }
                        if sending() {
                            if let Some(activity) = chat_turn().filter(|turn| Some(turn.chat_id) == current_chat_id()).and_then(|turn| turn.activity) {
                                div { class: "history-activity", role: "status", aria_live: "polite",
                                    "data-activity": if activity == ChatActivity::SearchingChats { "searching" } else { "reading" },
                                    RememberingIndicator {}
                                }
                            } else { div {
                                class: "thinking",
                                aria_label: "Courtside is preparing a reply",
                                span {}
                                span {}
                                span {}
                                span {}
                                span {}
                            } }
                        }
                        div {
                            class: "chat-scroll-anchor",
                            onmounted: move |_| chat_mounted.set(true),
                        }
                    }
                }
            }
            div { class: "chat-dock",
                if let Some((call_id, request, remembering)) = pending_answer {
                    CallQuestionPrompt { key: "{request.id}", call_id, request, remembering }
                }
                if cfg!(target_os = "ios") {
                    button {
                        id: "courtside-native-contact-action",
                        class: "native-menu-action",
                        r#type: "button",
                        tabindex: "-1",
                        aria_hidden: "true",
                        onclick: move |_| choose_contact(),
                    }
                    button {
                        id: "courtside-native-model-action",
                        class: "native-menu-action",
                        r#type: "button",
                        tabindex: "-1",
                        aria_hidden: "true",
                        onclick: move |_| {
                            let current = draft();
                            if current.split_whitespace().last().unwrap_or("").to_ascii_lowercase().starts_with("/model") {
                                draft.set(strip_trailing_tool_command(&current).0);
                            }
                        },
                    }
                }
                if let Some(query) = inline_contact_query.clone() {
                    div { class: "contact-suggestions", role: "menu",
                        span { class: "contact-suggestions-label", "CONTACTS" }
                        if query.is_empty() {
                            div { class: "contact-search-status", "Type a name after @" }
                        } else if contacts_loading() {
                            div { class: "contact-search-status", "Searching for “{query}”…" }
                        } else if !contacts_error().is_empty() {
                            div { class: "contact-search-status error", "{contacts_error}" }
                        } else if contact_matches().is_empty() {
                            div { class: "contact-search-status", "No contacts matching “{query}”" }
                        } else {
                            for contact in contact_matches() {
                                {
                                    let selected_contact = contact.clone();
                                    rsx! {
                                        button {
                                            class: "contact-result",
                                            r#type: "button",
                                            role: "menuitem",
                                            onclick: move |_| {
                                                draft.set(draft_with_contact(&draft(), &selected_contact));
                                            },
                                            span { class: "contact-avatar",
                                                "{contact.name.chars().next().unwrap_or('C').to_uppercase()}"
                                            }
                                            span { class: "tool-copy",
                                                strong { "{contact.name}" }
                                                small { "{contact.phone_number}" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        button {
                            class: "browse-contacts",
                            r#type: "button",
                            role: "menuitem",
                            onclick: move |_| choose_contact(),
                            span { class: "tool-icon", Icon { kind: IconKind::Contacts, size: 18 } }
                            span { class: "tool-copy",
                                strong { "Browse all contacts" }
                                small { "Use the system contact picker" }
                            }
                        }
                    }
                }
                if !send_error().is_empty() {
                    p { class: "composer-error", aria_live: "polite", "{send_error}" }
                }
                div { class: "composer-row",
                    form {
                        class: "composer",
                        onsubmit: move |event| {
                            event.prevent_default();
                            send();
                        },
                        textarea {
                            id: "courtside-composer-input",
                            rows: "1",
                            enterkeyhint: "enter",
                            autocomplete: "off",
                            autocapitalize: "sentences",
                            maxlength: "2000",
                            aria_label: "Message Courtside",
                            placeholder: "Plan or ask about a call…",
                            value: "{draft}",
                            onmounted: move |_| {
                                spawn(async move {
                                    let _ = document::eval("window.courtsideLayout?.schedule();").await;
                                });
                            },
                            oninput: move |event| {
                                send_error.set(String::new());
                                draft.set(event.value());
                            },
                            onkeydown: move |event| {
                                if event.key() == Key::Enter
                                    && !event.is_composing()
                                    && ((cfg!(target_arch = "wasm32") && !event.modifiers().intersects(Modifiers::SHIFT))
                                        || event.modifiers().intersects(Modifiers::CONTROL | Modifiers::META))
                                {
                                    event.prevent_default();
                                    send();
                                }
                            },
                        }
                        div { class: "composer-actions",
                            if cfg!(target_arch = "wasm32") {
                                details { class: "browser-tools",
                                    summary { id: "courtside-tools-button", aria_label: "Call planning shortcuts", "+" }
                                    div { class: "browser-tools-menu",
                                        button { r#type: "button", onclick: move |_| draft.set("Help me plan a restaurant reservation".into()), "Restaurant reservation" }
                                        button { r#type: "button", onclick: move |_| draft.set("Help me schedule an appointment".into()), "Schedule an appointment" }
                                        button { r#type: "button", onclick: move |_| draft.set("Help me plan a follow-up call".into()), "Follow up on a call" }
                                    }
                                }
                            } else { button {
                                id: "courtside-tools-button",
                                class: "composer-tools-button",
                                r#type: "button",
                                aria_label: "Add to message",
                                aria_haspopup: "menu",
                                aria_hidden: if cfg!(target_os = "ios") { "true" } else { "false" },
                                tabindex: if cfg!(target_os = "ios") { "-1" } else { "0" },
                                title: "Add to message",
                                disabled: sending() || contact_picker_open(),
                                onclick: move |_| choose_contact(),
                                Icon { kind: IconKind::Plus, size: 25 }
                            } }
                            if cfg!(target_os = "ios") {
                                button {
                                    id: "courtside-model-button",
                                    class: "composer-model-button",
                                    r#type: "button",
                                    aria_label: "Choose call model",
                                    aria_haspopup: "menu",
                                    aria_hidden: "true",
                                    tabindex: "-1",
                                    title: "Choose call model",
                                    span { "GPT Live" }
                                }
                            } else if cfg!(target_arch = "wasm32") {
                                span { class: "composer-model-button", aria_label: "Call model: GPT Live", "GPT Live" }
                            } else {
                                select {
                                    id: "courtside-model-button",
                                    class: "composer-model-button",
                                    aria_label: "Choose call model",
                                    option { value: "gpt-live", "GPT Live" }
                                }
                            }
                            button {
                                class: "send-button",
                                r#type: "submit",
                                aria_label: "Send message",
                                disabled: sending() || draft().trim().is_empty(),
                                Icon { kind: IconKind::ArrowUp, size: 22 }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn question_seconds_remaining(
    expires_at: chrono::DateTime<Utc>,
    now: chrono::DateTime<Utc>,
) -> u64 {
    (expires_at
        .signed_duration_since(now)
        .num_milliseconds()
        .max(0) as u64)
        .div_ceil(1_000)
}

#[component]
fn CallQuestionPrompt(
    call_id: Uuid,
    request: CallQuestion,
    #[props(default)] remembering: bool,
) -> Element {
    let mut answer = use_signal(String::new);
    let mut sending = use_signal(|| false);
    let mut submitted = use_signal(|| false);
    let mut error = use_signal(String::new);
    let question_id = request.id;
    let mut now = use_signal(Utc::now);
    let expires_at = request.expires_at;
    use_future(move || async move {
        while Utc::now() < expires_at {
            // A local clock updates only this component. No viewport changes,
            // whole-chat rerenders, or network request on every countdown tick.
            if document::eval(
                "await new Promise(resolve => setTimeout(resolve, 250)); return true;",
            )
            .await
            .is_err()
            {
                break;
            }
            now.set(Utc::now());
        }
    });
    let remaining = question_seconds_remaining(expires_at, now());
    if remaining == 0 || submitted() {
        return rsx! {};
    }
    let clock = format!("{}:{:02}", remaining / 60, remaining % 60);
    rsx! {
        section { class: "call-question-prompt",
            div { class: "call-question-heading",
                strong { if remembering { "Checking the detail" } else { "Your answer is needed" } }
                span { class: "call-question-clock", role: "timer", aria_live: "off", "{clock}" }
            }
            p { "{request.question}" }
            p { class: "call-question-hint",
                if remembering { "Checking earlier chats. You can answer here while I look." }
                else { "Answer here before the timer ends. The caller is waiting." }
            }
            form { class: "call-answer-form",
                onsubmit: move |event| {
                    event.prevent_default();
                    let content = answer().trim().to_string();
                    if sending() || content.is_empty() || Utc::now() >= expires_at { return; }
                    sending.set(true);
                    error.set(String::new());
                    // The server clears this question as soon as it accepts the
                    // answer. Keep its HTTP request alive after the card unmounts.
                    dioxus::core::spawn_forever(async move {
                        let response = api::send_call_answer(content, CallAnswerTarget {call_id, question_id}).await;
                        match response {
                            Ok(_) => { if let Ok(mut state) = submitted.try_write() { *state = true; } },
                            Err(message) => { if let Ok(mut state) = error.try_write() { *state = message; } },
                        }
                        if let Ok(mut state) = sending.try_write() { *state = false; }
                    });
                },
                input { r#type: "text", aria_label: "Answer the caller", placeholder: "Type your answer…",
                    value: "{answer}", maxlength: "600", disabled: sending(),
                    autocomplete: "off", oninput: move |event| answer.set(event.value()),
                }
                button { r#type: "submit", disabled: sending() || answer().trim().is_empty(),
                    if sending() { "Sending…" } else { "Send answer" }
                }
            }
            if !error().is_empty() { p { class: "call-answer-error", role: "alert", "{error}" } }
        }
    }
}

#[component]
fn LiveCallCard(
    feed: LiveCallTranscript,
    peer_number: String,
    mut live_call: Signal<Option<LiveCallTranscript>>,
) -> Element {
    let connected = matches!(feed.status, CallStatus::InProgress);
    let active = matches!(
        feed.status,
        CallStatus::Queued | CallStatus::Ringing | CallStatus::InProgress
    );
    let call_id = feed.call_id;
    let mode = feed.mode;
    let mut voice_status = use_context::<Signal<Option<voice::Event>>>();
    let listen_status = use_context::<Signal<Option<listen::Event>>>();
    let native_status = voice_status().filter(|event| event.call_id == call_id);
    let joining = native_status
        .as_ref()
        .is_some_and(|event| event.phase == voice::Phase::Connecting);
    let mut listening = use_signal(|| false);
    let mut microphone_muted = use_signal(|| false);
    let mut control_busy = use_signal(|| false);
    let mut control_message = use_signal(String::new);
    let mut transcript_mounted = use_signal(|| false);
    let mut revealed_control_message = use_signal(String::new);
    let displayed_message = if !control_message().is_empty() {
        control_message()
    } else if let Some(event) = native_status
        .as_ref()
        .filter(|event| matches!(event.phase, voice::Phase::Connecting | voice::Phase::Error))
    {
        event.message.clone()
    } else {
        feed.control_message.clone()
    };
    let state = match feed.status {
        CallStatus::Queued => "Starting",
        CallStatus::Ringing => "Ringing",
        CallStatus::InProgress => "Live",
        CallStatus::Completed => "Completed",
        CallStatus::Failed => "Call failed",
    };
    use_effect(move || {
        let current = live_call().filter(|feed| feed.call_id == call_id);
        if current.as_ref().is_some_and(|feed| {
            feed.status != CallStatus::InProgress || feed.mode != CallMode::Assistant
        }) && listening()
        {
            stop_live_listen(call_id);
            listening.set(false);
        }
    });
    use_effect(move || {
        if let Some(event) = listen_status().filter(|event| event.call_id == call_id) {
            listening.set(matches!(
                event.phase,
                voice::Phase::Connecting | voice::Phase::Connected
            ));
            control_message.set(event.message);
        }
    });
    use_effect(move || {
        if voice_status().is_some_and(|event| {
            event.call_id == call_id && event.phase != voice::Phase::Connecting
        }) {
            control_message.set(String::new());
        }
        if !voice_status()
            .is_some_and(|event| event.call_id == call_id && event.phase == voice::Phase::Connected)
        {
            microphone_muted.set(false);
        }
    });
    use_effect(move || {
        let _latest_transcript_revision = live_call()
            .filter(|current| current.call_id == call_id)
            .and_then(|current| {
                current
                    .lines
                    .last()
                    .map(|line| (line.end_ms, line.text.len()))
            });
        if transcript_mounted() {
            spawn(async move {
                let _ = document::eval(&format!(
                    "window.courtsideLayout?.scrollToEnd('courtside-live-transcript-{call_id}');"
                ))
                .await;
            });
        }
    });
    use_drop(move || stop_live_listen(call_id));
    use_effect(move || {
        let local_message = control_message();
        let server_message = live_call()
            .filter(|feed| feed.call_id == call_id)
            .map(|feed| feed.control_message)
            .unwrap_or_default();
        let message = if local_message.is_empty() {
            server_message
        } else {
            local_message
        };
        if message != *revealed_control_message.peek() {
            revealed_control_message.set(message.clone());
        } else {
            return;
        }
        if !message.is_empty() {
            spawn(async move {
                let _ = document::eval(&format!(
                    "window.courtsideLayout?.revealCallControl('courtside-call-control-status-{call_id}');"
                )).await;
            });
        }
    });

    rsx! {
        article { class: if connected { "live-call-card connected" } else { "live-call-card" },
            header { class: "live-call-heading",
                div { class: "live-call-title",
                    span { class: "live-call-dot" }
                    strong { "{state} call" }
                }
                span { class: "live-call-number", "{peer_number}" }
            }
            if feed.remembering && connected && mode == CallMode::Assistant {
                div { class: "history-activity call-remembering", role: "status", aria_live: "polite",
                    RememberingIndicator {}
                }
            }
            if feed.lines.is_empty() {
                p { class: "live-call-waiting",
                    if active { "Waiting for the conversation…" } else { "No conversation was captured." }
                }
            } else {
                div {
                    id: "courtside-live-transcript-{call_id}",
                    class: "live-transcript",
                    aria_live: "polite",
                    for line in feed.lines {
                        div {
                            class: if line.speaker == Speaker::Action {
                                "live-transcript-line action"
                            } else {
                                "live-transcript-line"
                            },
                            span {
                                match line.speaker {
                                    Speaker::Assistant => "Courtside",
                                    Speaker::Callee => "Other party",
                                    Speaker::User => "You",
                                    Speaker::Action => "Keypad",
                                }
                            }
                            p { "{line.text}" }
                        }
                    }
                    div {
                        class: "live-transcript-scroll-anchor",
                        onmounted: move |_| transcript_mounted.set(true),
                    }
                }
            }
            if active {
                div { class: "call-controls",
                    button {
                        class: "call-control danger",
                        r#type: "button",
                        disabled: control_busy(),
                        onclick: move |_| {
                            if control_busy() { return; }
                            control_busy.set(true);
                            control_message.set("Ending call…".into());
                            spawn(async move {
                                match api::end_call(call_id).await {
                                    Ok(_) => {
                                        stop_live_listen(call_id);
                                        voice::end(Some(call_id));
                                        control_message.set("Ending call…".into());
                                    },
                                    Err(error) => control_message.set(error),
                                }
                                control_busy.set(false);
                            });
                        },
                        Icon { kind: IconKind::Stop, size: 17 }
                        "End call"
                    }
                    button {
                        class: if listening() { "call-control active" } else { "call-control" },
                        r#type: "button",
                        disabled: !connected || mode != CallMode::Assistant || joining,
                        aria_pressed: if listening() { "true" } else { "false" },
                        onclick: move |_| {
                            if listening() {
                                stop_live_listen(call_id);
                                listening.set(false);
                            } else {
                                voice_status.set(None);
                                match start_live_listen(call_id) {
                                    Ok(()) => {
                                        listening.set(true);
                                        control_message.set("Connecting live audio…".into());
                                    }
                                    Err(error) => control_message.set(error),
                                }
                            }
                        },
                        Icon { kind: IconKind::Headphones, size: 17 }
                        if listening() { "Listening" } else { "Live listen" }
                    }
                    button {
                        class: if mode != CallMode::Assistant { "call-control active" } else { "call-control" },
                        r#type: "button",
                        disabled: control_busy() || (joining && !cfg!(target_arch = "wasm32")),
                        onclick: move |_| {
                            if control_busy() { return; }
                            if joining {
                                if cfg!(target_arch = "wasm32") { voice::end(Some(call_id)); }
                                return;
                            }
                            stop_live_listen(call_id);
                            listening.set(false);
                            if mode == CallMode::InAppTakeover
                                && voice_status().is_some_and(|event| event.call_id == call_id && event.phase == voice::Phase::Connected) {
                                control_message.set("Returning the call to Courtside…".into());
                                voice::end(Some(call_id));
                                return;
                            }
                            control_busy.set(true);
                            let peer = peer_number.clone();
                            spawn(async move {
                                if mode == CallMode::Assistant {
                                    control_message.set(String::new());
                                    voice_status.set(Some(voice::Event { call_id, phase: voice::Phase::Connecting,
                                        message: "Joining the call…".into() }));
                                    let result = match api::call_detail(call_id).await {
                                        Ok(detail) if detail.call.status == CallStatus::InProgress => voice::start(call_id, &peer),
                                        Ok(_) => Err("The other person hasn’t answered yet, or the call has ended.".into()),
                                        Err(error) => Err(error),
                                    };
                                    if let Err(error) = result {
                                        voice_status.set(Some(voice::Event { call_id, phase: voice::Phase::Error, message: error }));
                                    }
                                } else {
                                    control_message.set("Returning the call to Courtside…".into());
                                    match api::resume_assistant(call_id).await {
                                        Ok(response) => {
                                            live_call.with_mut(|current| {
                                                if let Some(current) = current.as_mut().filter(|item| item.call_id == call_id) {
                                                    current.mode = response.mode;
                                                    current.control_message = response.message.clone();
                                                }
                                            });
                                            control_message.set(String::new());
                                            voice_status.set(None);
                                        }
                                        Err(error) => control_message.set(error),
                                    }
                                }
                                control_busy.set(false);
                            });
                        },
                        Icon { kind: IconKind::User, size: 17 }
                        match mode {
                            CallMode::Assistant => if joining { if cfg!(target_arch = "wasm32") { "Cancel joining" } else { "Joining…" } } else if control_busy() { "Please wait…" } else { "Take over" },
                            CallMode::TakeoverPending => "Cancel takeover",
                            CallMode::Takeover | CallMode::InAppTakeover => "Return to AI",
                        }
                    }
                    if cfg!(target_arch = "wasm32") && native_status.as_ref().is_some_and(|event| event.phase == voice::Phase::Connected) {
                        button {
                            class: if microphone_muted() { "call-control active" } else { "call-control" },
                            r#type: "button",
                            aria_pressed: if microphone_muted() { "true" } else { "false" },
                            onclick: move |_| {
                                let muted = !microphone_muted();
                                voice::set_muted(call_id, muted);
                                microphone_muted.set(muted);
                            },
                            if microphone_muted() { "Unmute mic" } else { "Mute mic" }
                        }
                    }
                }
                if !displayed_message.is_empty() {
                    p {
                        id: "courtside-call-control-status-{call_id}",
                        class: "call-control-message",
                        role: "status",
                        "{displayed_message}"
                    }
                }
            }
        }
    }
}

#[component]
fn CallsScreen(
    calls: Signal<Vec<Call>>,
    calls_error: Signal<String>,
    mut calls_revision: Signal<u32>,
    mut pane: Signal<Pane>,
    drawer_open: Signal<bool>,
) -> Element {
    let filter = use_signal(|| CallFilter::All);
    let selected_calls = calls()
        .into_iter()
        .filter(|call| match filter() {
            CallFilter::All => true,
            CallFilter::Active => matches!(
                call.status,
                CallStatus::Queued | CallStatus::Ringing | CallStatus::InProgress
            ),
            CallFilter::Completed => {
                matches!(call.status, CallStatus::Completed | CallStatus::Failed)
            }
        })
        .collect::<Vec<_>>();

    rsx! {
        section { class: "screen standard-screen",
            ScreenHeader { title: "Calls", subtitle: "Let Courtside make the calls.", drawer_open }
            div { class: "filter-row",
                FilterButton { title: "All", target: CallFilter::All, filter }
                FilterButton { title: "In Progress", target: CallFilter::Active, filter }
                FilterButton { title: "Completed", target: CallFilter::Completed, filter }
            }
            if !calls_error().is_empty() {
                button { class: "inline-error", onclick: move |_| *calls_revision.write() += 1,
                    "{calls_error} Tap to retry."
                }
            }
            if selected_calls.is_empty() {
                div { class: "empty-calls",
                    div { class: "empty-call-icon",
                        Icon { kind: IconKind::File, size: 38 }
                        span { Icon { kind: IconKind::Clock, size: 21 } }
                    }
                    h2 { "No calls yet" }
                    p { "Start one and Courtside will handle the conversation." }
                    button { class: "primary-button compact-primary", onclick: move |_| pane.set(Pane::Assistant), "Ask Courtside" }
                }
            } else {
                div { class: "call-list",
                    for call in selected_calls {
                        CallCard { key: "{call.id}", call }
                    }
                }
            }
        }
    }
}

#[component]
fn FilterButton(
    title: &'static str,
    target: CallFilter,
    mut filter: Signal<CallFilter>,
) -> Element {
    rsx! {
        button {
            class: if filter() == target { "filter active" } else { "filter" },
            onclick: move |_| filter.set(target),
            "{title}"
        }
    }
}

#[component]
fn CallCard(call: Call) -> Element {
    let summary = call
        .summary
        .clone()
        .or(call.task_prompt.clone())
        .unwrap_or_else(|| "Courtside phone call".into());
    rsx! {
        article { class: "call-card",
            div { class: "call-icon", Icon { kind: IconKind::Phone, size: 20 } }
            div { class: "call-copy",
                strong { "{call.peer_number}" }
                p { "{summary}" }
            }
            span { class: "call-status", "{status_label(call.status)}" }
        }
    }
}

#[component]
fn AccountScreen(
    user: UserProfile,
    mut session_state: Signal<Option<Session>>,
    mut pane: Signal<Pane>,
    drawer_open: Signal<bool>,
) -> Element {
    let account_revision = use_signal(|| 0_u32);
    let mut verifying = use_signal(|| false);
    let mut claiming_number = use_signal(|| false);
    let account = use_resource(move || {
        let _ = account_revision();
        async move { api::account_status().await }
    });
    let preferences = use_resource(move || api::call_preferences());
    if cfg!(target_arch = "wasm32") {
        if let Some(Ok(status)) = account() {
            if verifying() && !status.phone_verified {
                return rsx! { CallerIdVerification {
                    revision: account_revision, session_state,
                    on_cancel: Some(EventHandler::new(move |_| verifying.set(false))),
                } };
            }
            if claiming_number() && status.needs_number_claim {
                return rsx! { NumberClaim {
                    revision: account_revision, session_state,
                    on_cancel: Some(EventHandler::new(move |_| claiming_number.set(false))),
                } };
            }
        }
    }
    rsx! {
        section { class: "screen standard-screen account-screen",
            ScreenHeader { title: "Account", subtitle: "Your number, plan, and usage.", drawer_open }
            div { class: "account-profile panel",
                Avatar { name: user.display_name.clone(), size: 52 }
                div { strong { "{user.display_name}" } p { "{user.email}" } }
            }
            span { class: "section-label", "PLAN & MINUTES" }
            match account() {
                None => rsx! { div { class: "panel", "Loading account…" } },
                Some(Err(error)) => rsx! { div { class: "panel error-panel", "{error}" } },
                Some(Ok(status)) => rsx! { AccountSummary { status } },
            }
            if cfg!(target_arch = "wasm32") {
                if let Some(Ok(status)) = account() {
                    if !status.phone_verified {
                        div { class: "panel web-verification-panel",
                            strong { "Set up your caller ID" }
                            p { "You can plan calls now. Verify your mobile number before Courtside places a real call." }
                            button { class: "primary-button compact-primary", onclick: move |_| verifying.set(true), "Verify my number" }
                        }
                    } else if status.needs_number_claim {
                        div { class: "panel web-verification-panel",
                            strong { "Choose your dedicated number" }
                            p { "Choose a number for calls on your current plan." }
                            button { class: "primary-button compact-primary", onclick: move |_| claiming_number.set(true), "Choose my number" }
                        }
                    }
                }
            }
            span { class: "section-label", "CALL BEHAVIOR" }
            if let Some(Ok(status)) = account() {
                div { class: "panel incoming-setup",
                    strong { "Incoming call screening" }
                    if let Some(number) = status.dedicated_number.filter(|number| number.inbound_enabled) {
                        p { "Your AI answers calls to {number.phone_number}. Forward your mobile calls here to have Courtside answer for you." }
                        p { "Courtside asks who's calling, posts live updates, and invites you to join if the caller asks for you. If you don't join, it takes a message." }
                    } else {
                        p { "Incoming calls need a dedicated Courtside number on a paid plan. Your carrier can forward calls from your usual number to it." }
                    }
                    button { class: "primary-button compact-primary", r#type: "button",
                        onclick: move |_| pane.set(Pane::Forwarding),
                        "Set up call forwarding"
                    }
                    if cfg!(target_os = "ios") {
                        small { "In this development build, keep Courtside open to receive join invitations. Screening and saved messages continue on the server while the app is closed." }
                    }
                }
            }
            match preferences() {
                None => rsx! { div { class: "panel", "Loading call preferences…" } },
                Some(Err(error)) => rsx! { div { class: "panel error-panel", "{error}" } },
                Some(Ok(initial)) => rsx! {
                    CallPreferencesEditor { initial, user_name: user.display_name.clone() }
                },
            }
            ChatHistoryPreference {}
            if !cfg!(target_arch = "wasm32") { div { class: "panel migration-note",
                strong { "Native purchase bridge" }
                p { "StoreKit restore and upgrade controls will be reconnected after the Dioxus client is verified on-device." }
            }
            div { class: "panel developer-panel",
                span { "Connected server" }
                code { "{api::base_url()}" }
            } }
            button { class: "signout-button", onclick: move |_| sign_out(&mut session_state),
                Icon { kind: IconKind::Logout, size: 20 }
                "Sign Out"
            }
        }
    }
}

#[component]
fn ForwardingScreen(
    mut pane: Signal<Pane>,
    drawer_open: Signal<bool>,
    session_state: Signal<Option<Session>>,
) -> Element {
    let mut revision = use_signal(|| 0_u32);
    let mut claiming = use_signal(|| false);
    let mut missed_only = use_signal(|| false);
    let account = use_resource(move || {
        let _ = revision();
        async move { api::account_status().await }
    });
    if claiming()
        && account().is_some_and(|status| {
            status.is_ok_and(|status| {
                status.tier == AccountTier::Paid && status.dedicated_number.is_none()
            })
        })
    {
        return rsx! {
            NumberClaim {
                revision, session_state,
                on_cancel: Some(EventHandler::new(move |_| claiming.set(false))),
                cancel_label: "Back to forwarding",
            }
        };
    }
    rsx! {
        section { class: "screen standard-screen forwarding-screen",
            ScreenHeader { title: "Call forwarding", subtitle: "Let your AI answer incoming calls.", drawer_open }
            match account() {
                None => rsx! { div { class: "panel", role: "status", "Loading your Courtside number…" } },
                Some(Err(error)) => rsx! {
                    div { class: "panel error-panel",
                        p { "{error}" }
                        button { class: "text-button", onclick: move |_| *revision.write() += 1, "Try again" }
                    }
                },
                Some(Ok(status)) if status.tier != AccountTier::Paid => rsx! {
                    div { class: "panel forwarding-intro",
                        h2 { "Activate incoming calls" }
                        p { "AI call answering needs an active paid plan and a dedicated Courtside number. Open Account to check your plan before forwarding your mobile calls." }
                        button { class: "primary-button compact-primary", r#type: "button",
                            onclick: move |_| pane.set(Pane::Account), "Open Account"
                        }
                    }
                },
                Some(Ok(status)) if status.included_seconds_used >= status.included_seconds_limit => rsx! {
                    div { class: "panel forwarding-intro",
                        h2 { "Incoming calls are paused" }
                        p { "You've used the included call minutes for this period. Check your plan in Account before enabling forwarding." }
                        button { class: "primary-button compact-primary", r#type: "button",
                            onclick: move |_| pane.set(Pane::Account), "Open Account"
                        }
                    }
                },
                Some(Ok(status)) => match status.dedicated_number {
                    None => rsx! {
                        div { class: "panel forwarding-intro",
                            Icon { kind: IconKind::Phone, size: 30 }
                            h2 { "Get your AI's number first" }
                            p { "Forwarding needs a dedicated Courtside number that can receive calls. Dedicated numbers are included with a paid plan." }
                            p { "Your mobile number stays with your carrier. Once your Courtside number is ready, come back here to set up forwarding." }
                            button { class: "primary-button compact-primary", r#type: "button",
                                onclick: move |_| claiming.set(true), "Choose my Courtside number"
                            }
                        }
                    },
                    Some(number) if number.inbound_enabled => rsx! {
                        div { class: "panel forwarding-intro",
                            span { class: "section-label", "FORWARD TO YOUR AI" }
                            CopyForwardingNumber { number: number.phone_number.clone() }
                            p { "Courtside asks who's calling, sends you live updates, and invites you to join when the caller asks for you." }
                        }
                        div { class: "forwarding-modes", role: "group", aria_label: "Forwarding setup guide",
                            button { r#type: "button", aria_pressed: (!missed_only()).to_string(),
                                onclick: move |_| missed_only.set(false),
                                strong { "All calls" }
                                span { "Your AI answers first" }
                            }
                            button { r#type: "button", aria_pressed: missed_only().to_string(),
                                onclick: move |_| missed_only.set(true),
                                strong { "Missed calls" }
                                span { "Your phone rings first" }
                            }
                        }
                        div { class: "panel forwarding-steps",
                            h2 { if missed_only() { "Forward when you can't answer" } else { "Set up on your iPhone" } }
                            if missed_only() {
                                p { "Ask your carrier to enable conditional forwarding for unanswered, busy, or unreachable calls to your Courtside number above." }
                                p { "Use your carrier's setup instructions and dial codes. This is separate from the iPhone's all-calls forwarding switch." }
                                p { "For Helium Mobile, ask Helium support which conditional forwarding options your plan supports." }
                            } else {
                                ol {
                                    li { "Copy your Courtside number above." }
                                    li { "Open Settings → Apps → Phone → Call Forwarding. On a Dual SIM iPhone, choose your mobile line." }
                                    li { "Turn on Call Forwarding, tap Forward To, and paste your Courtside number. Tap Back to save." }
                                }
                                p { "If Call Forwarding is missing, ask your carrier how to forward all calls to this number. On other phones, use your carrier's call-forwarding settings." }
                            }
                            a { class: "forwarding-guide-link", href: "https://support.apple.com/guide/iphone/iph7405291c4/ios",
                                target: "_blank", rel: "noopener noreferrer", "Apple's forwarding guide ↗"
                            }
                            p { class: "forwarding-note", "Choose the guide here, then enable forwarding with your carrier. Courtside can't change or read your carrier's forwarding setting." }
                        }
                        div { class: "panel forwarding-steps",
                            h2 { "Check that it works" }
                            p { "From another phone, call your usual mobile number. For missed-call forwarding, let it ring without answering. Courtside should answer and the incoming call will appear in Calls." }
                            button { class: "primary-button compact-primary", r#type: "button",
                                onclick: move |_| pane.set(Pane::Calls), "View incoming calls"
                            }
                            if cfg!(target_os = "ios") {
                                p { class: "forwarding-note", "Keep Courtside open to receive join invitations in this development build. The AI can still screen calls and save messages while the app is closed." }
                            }
                        }
                        div { class: "panel forwarding-steps",
                            h2 { "Turn forwarding off" }
                            p { if missed_only() { "Use your carrier's instructions to restore its normal missed-call or voicemail handling." } else { "Return to Settings → Apps → Phone → Call Forwarding and turn it off. If you used carrier dial codes, follow your carrier's cancellation instructions." } }
                        }
                    },
                    Some(_) => rsx! {
                        div { class: "panel forwarding-intro",
                            h2 { "Your number isn't ready for incoming calls" }
                            p { "Your Courtside number must support incoming calls before you forward your mobile calls to it. Check your number in Account." }
                            button { class: "primary-button compact-primary", r#type: "button",
                                onclick: move |_| pane.set(Pane::Account), "Open Account"
                            }
                        }
                    },
                },
            }
        }
    }
}

#[component]
fn CopyForwardingNumber(number: String) -> Element {
    let mut message = use_signal(String::new);
    let copy_value = number.clone();
    rsx! {
        div { class: "forwarding-number",
            input { r#type: "text", readonly: true, value: "{number}", aria_label: "Courtside forwarding number" }
            button { class: "forwarding-copy", r#type: "button",
                onclick: move |_| {
                    let value = copy_value.clone();
                    message.set(String::new());
                    spawn(async move {
                        #[cfg(target_os = "ios")]
                        {
                            crate::native::copy_text(value);
                            message.set("Number copied. Paste it into your forwarding settings.".into());
                        }
                        #[cfg(not(target_os = "ios"))]
                        {
                            let value = serde_json::to_string(&value).unwrap_or_default();
                            let script = format!(r#"
                                const value = {value};
                                try {{ await navigator.clipboard.writeText(value); return true; }} catch (_) {{
                                    const input = document.querySelector('[aria-label="Courtside forwarding number"]');
                                    input.focus(); input.select();
                                    return document.execCommand('copy');
                                }}
                            "#);
                            let copied = document::eval(&script).await.is_ok_and(|value| value == true);
                            message.set(if copied { "Number copied. Paste it into your forwarding settings." } else { "Select the number and copy it to your forwarding settings." }.into());
                        }
                    });
                },
                "Copy number"
            }
        }
        if !message().is_empty() {
            p { class: "forwarding-copy-status", role: "status", "{message}" }
        }
    }
}

#[component]
fn ChatHistoryPreference() -> Element {
    let saved = use_resource(move || api::history_settings());
    match saved() {
        None => rsx! { div { class: "panel", "Loading chat preferences…" } },
        Some(Err(error)) => rsx! { div { class: "panel error-panel", "{error}" } },
        Some(Ok(initial)) => rsx! { HistoryPreference {initial} },
    }
}

#[component]
fn HistoryPreference(initial: HistorySettingsResponse) -> Element {
    let mut enabled = use_signal(|| initial.enabled);
    let mut busy = use_signal(|| false);
    let mut message = use_signal(String::new);
    rsx! {
        div { class: "panel history-preference",
            div { class: "preference-row",
                div {
                    strong { "Reference past chats" }
                    p { "Look through earlier conversations when a chat or call needs more context." }
                }
                button { r#type: "button", role: "switch", aria_label: "Reference past chats",
                    aria_checked: if enabled() { "true" } else { "false" },
                    class: if enabled() { "preference-switch on" } else { "preference-switch" },
                    disabled: busy(),
                    onclick: move |_| {
                        let next = !enabled();
                        busy.set(true);
                        spawn(async move {
                            match api::set_history_enabled(next).await {
                                Ok(saved) => { enabled.set(saved.enabled); message.set(String::new()); },
                                Err(error) => message.set(error),
                            }
                            busy.set(false);
                        });
                    },
                }
            }
            if !message().is_empty() { p { role: "status", "{message}" } }
        }
    }
}

#[component]
fn CallPreferencesEditor(initial: CallPreferences, user_name: String) -> Element {
    let mut voice = use_signal(|| initial.voice);
    let mut announce_courtside = use_signal(|| initial.announce_courtside);
    let mut custom_instructions = use_signal(|| initial.custom_instructions.clone());
    let mut takeover_phone = use_signal(|| initial.takeover_phone_number.clone());
    let mut saving = use_signal(|| false);
    let mut message = use_signal(String::new);

    let mut save = move || {
        if saving() {
            return;
        }
        saving.set(true);
        message.set(String::new());
        let selected_voice = voice();
        let announce = announce_courtside();
        let custom = custom_instructions();
        let phone = takeover_phone();
        spawn(async move {
            match api::update_call_preferences(selected_voice, announce, custom, phone).await {
                Ok(saved) => {
                    voice.set(saved.voice);
                    announce_courtside.set(saved.announce_courtside);
                    custom_instructions.set(saved.custom_instructions);
                    takeover_phone.set(saved.takeover_phone_number);
                    message.set("Saved for future calls.".into());
                }
                Err(error) => message.set(error),
            }
            saving.set(false);
        });
    };

    rsx! {
        form {
            class: "panel call-preferences",
            onsubmit: move |event| {
                event.prevent_default();
                save();
            },
            div { class: "voice-setting",
                div { class: "preference-heading",
                    strong { "Assistant voice" }
                    p { "Used for every GPT Live call." }
                }
                VoicePicker {
                    selected: voice(),
                    on_select: move |selected| {
                        voice.set(selected);
                        message.set(String::new());
                    },
                }
            }
            div { class: "preference-row",
                div {
                    strong { "Courtside introduction" }
                    p { "Identify the assistant before each outbound call." }
                }
                button {
                    r#type: "button",
                    class: if announce_courtside() { "preference-switch on" } else { "preference-switch" },
                    role: "switch",
                    aria_checked: if announce_courtside() { "true" } else { "false" },
                    aria_label: "Use the Courtside introduction",
                    onclick: move |_| {
                        let next = !announce_courtside();
                        announce_courtside.set(next);
                        message.set(String::new());
                    },
                    span {}
                }
            }
            p { class: "greeting-preview",
                if announce_courtside() {
                    "“Hello, this is the Courtside assistant calling on behalf of {user_name}.”"
                } else {
                    "Calls will begin directly with their purpose."
                }
            }
            label {
                "Custom call instructions"
                textarea {
                    maxlength: "800",
                    rows: "3",
                    placeholder: "Example: Keep calls brief, speak slowly, and confirm details once.",
                    value: "{custom_instructions}",
                    oninput: move |event| {
                        custom_instructions.set(event.value());
                        message.set(String::new());
                    }
                }
            }
            label {
                "Call takeover"
                small { "Join live calls directly in Courtside with your microphone. Use Return to AI to let Courtside continue the conversation." }
            }
            div { class: "preference-save-row",
                if !message().is_empty() {
                    span { class: "preference-message", "{message}" }
                }
                button { class: "preference-save", r#type: "submit", disabled: saving(),
                    if saving() { "Saving…" } else { "Save" }
                }
            }
        }
    }
}

#[component]
fn AccountSummary(status: AccountStatusResponse) -> Element {
    let (title, used, limit) = match status.tier {
        AccountTier::Free => (
            "Free Trial",
            status.trial_seconds_used,
            status.trial_seconds_limit,
        ),
        AccountTier::Paid => (
            "Courtside Pro",
            status.included_seconds_used,
            status.included_seconds_limit,
        ),
    };
    let remaining = (limit - used).max(0);
    let minutes_left = (remaining + 59) / 60;
    let percentage = if limit > 0 {
        ((used as f64 / limit as f64) * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    let number = status
        .verified_phone_masked
        .clone()
        .unwrap_or_else(|| "Caller ID verification required".into());

    rsx! {
        div { class: "panel plan-card",
            div { class: "plan-heading",
                div { class: "plan-icon", Icon { kind: IconKind::Clock, size: 20 } }
                strong { "{title}" }
                span { "{minutes_left} min left" }
            }
            div { class: "usage-track", span { style: "width: {percentage}%" } }
            p { "Caller ID · {number}" }
        }
    }
}

#[component]
fn Drawer(
    user: UserProfile,
    recent_chats: Signal<Vec<ChatSummary>>,
    mut current_chat_id: Signal<Option<Uuid>>,
    mut pane: Signal<Pane>,
    mut drawer_open: Signal<bool>,
    mut chat_revision: Signal<u32>,
    mut chats_revision: Signal<u32>,
    chat_sending: Signal<bool>,
    mut live_chat_message: Signal<Option<ChatMessage>>,
    mut live_call: Signal<Option<LiveCallTranscript>>,
    mut chat_call_id: Signal<Option<Uuid>>,
) -> Element {
    let mut new_chat_busy = use_signal(|| false);
    let mut new_chat_error = use_signal(String::new);
    rsx! {
        aside {
            class: if drawer_open() { "drawer-overlay open" } else { "drawer-overlay" },
            aria_hidden: if cfg!(target_arch = "wasm32") || drawer_open() { "false" } else { "true" },
            button {
                class: "drawer-scrim",
                aria_label: "Close navigation",
                onclick: move |_| drawer_open.set(false),
            }
            nav { class: "drawer-panel",
                div { class: "drawer-inner",
                    div { class: "drawer-header",
                        div {
                            h1 { "Courtside" }
                            p { "Your phone assistant" }
                        }
                        button { class: "circle-button", aria_label: "Close navigation", onclick: move |_| drawer_open.set(false),
                            Icon { kind: IconKind::Close, size: 22 }
                        }
                    }
                    div { class: "drawer-links",
                        DrawerLink { icon: IconKind::Chat, title: "Assistant", target: Pane::Assistant, pane, drawer_open }
                        DrawerLink { icon: IconKind::Phone, title: "Calls", target: Pane::Calls, pane, drawer_open }
                        DrawerLink { icon: IconKind::Phone, title: "Call forwarding", target: Pane::Forwarding, pane, drawer_open }
                    }
                    div { class: "recent-section",
                        span { class: "section-label", "RECENT CHATS" }
                        if recent_chats().is_empty() {
                            p { class: "recent-empty", "Your conversations will appear here." }
                        } else {
                            for chat in recent_chats() {
                                button {
                                    class: if current_chat_id() == Some(chat.id) { "recent-chat active" } else { "recent-chat" },
                                    key: "{chat.id}",
                                    disabled: new_chat_busy() || chat_sending(),
                                    onclick: move |_| {
                                        if current_chat_id() == Some(chat.id) {
                                            pane.set(Pane::Assistant);
                                            drawer_open.set(false);
                                            return;
                                        }
                                        new_chat_busy.set(true);
                                        new_chat_error.set(String::new());
                                        spawn(async move {
                                            match api::select_chat(chat.id).await {
                                                Ok(response) => {
                                                    current_chat_id.set(Some(response.chat_id));
                                                    live_chat_message.set(None);
                                                    match response.call {
                                                        Some(detail) => {
                                                            chat_call_id.set(Some(detail.call.id));
                                                            live_call.set(Some(LiveCallTranscript::from_detail(detail)));
                                                        }
                                                        None => {
                                                            chat_call_id.set(None);
                                                            live_call.set(None);
                                                        }
                                                    }
                                                    *chat_revision.write() += 1;
                                                    pane.set(Pane::Assistant);
                                                    drawer_open.set(false);
                                                }
                                                Err(error) => new_chat_error.set(error),
                                            }
                                            new_chat_busy.set(false);
                                        });
                                    },
                                    i {}
                                    strong { "{chat.title}" }
                                }
                            }
                        }
                    }
                    div { class: "drawer-spacer" }
                    button {
                        class: "drawer-new-chat",
                        disabled: new_chat_busy() || chat_sending(),
                        onclick: move |_| {
                            if new_chat_busy() || chat_sending() {
                                return;
                            }
                            new_chat_busy.set(true);
                            new_chat_error.set(String::new());
                            spawn(async move {
                                match api::new_chat().await {
                                    Ok(response) => {
                                        current_chat_id.set(Some(response.chat_id));
                                        live_chat_message.set(None);
                                        live_call.set(None);
                                        chat_call_id.set(None);
                                        *chat_revision.write() += 1;
                                        *chats_revision.write() += 1;
                                        pane.set(Pane::Assistant);
                                        drawer_open.set(false);
                                    }
                                    Err(error) => new_chat_error.set(error),
                                }
                                new_chat_busy.set(false);
                            });
                        },
                        Icon { kind: IconKind::Plus, size: 22 }
                        if new_chat_busy() { "Starting…" } else { "New Chat" }
                    }
                    if !new_chat_error().is_empty() {
                        p { class: "drawer-action-error", "{new_chat_error}" }
                    }
                    button {
                        class: if pane() == Pane::Account { "drawer-profile active" } else { "drawer-profile" },
                        aria_label: "Open Account and Plan",
                        onclick: move |_| {
                            pane.set(Pane::Account);
                            drawer_open.set(false);
                        },
                        Avatar { name: user.display_name.clone(), size: 46 }
                        div { strong { "{user.display_name}" } span { "{user.email}" } }
                    }
                }
            }
        }
    }
}

#[component]
fn DrawerLink(
    icon: IconKind,
    title: &'static str,
    target: Pane,
    mut pane: Signal<Pane>,
    mut drawer_open: Signal<bool>,
) -> Element {
    rsx! {
        button {
            class: if pane() == target { "drawer-link active" } else { "drawer-link" },
            onclick: move |_| {
                pane.set(target);
                drawer_open.set(false);
            },
            Icon { kind: icon, size: 24 }
            "{title}"
        }
    }
}

#[component]
fn Logo(#[props(default = 44)] size: u16) -> Element {
    rsx! {
        img {
            class: "logo",
            width: "{size}",
            height: "{size}",
            src: COURTSIDE_MARK,
            alt: "Courtside",
        }
    }
}

#[component]
pub(crate) fn LiquidAquarium() -> Element {
    rsx! {
        div { class: "open-water", aria_hidden: "true",
            span { class: "water-ribbon ribbon-one" }
            span { class: "water-ribbon ribbon-two" }
            div { class: "fish fish-one",
                span { class: "fish-tail" }
                span { class: "fish-body",
                    i { class: "fish-fin" }
                    i { class: "fish-eye" }
                }
            }
            div { class: "fish fish-two",
                span { class: "fish-tail" }
                span { class: "fish-body",
                    i { class: "fish-fin" }
                    i { class: "fish-eye" }
                }
            }
            div { class: "fish fish-three",
                span { class: "fish-tail" }
                span { class: "fish-body",
                    i { class: "fish-fin" }
                    i { class: "fish-eye" }
                }
            }
            div { class: "bubble-field",
                span { class: "bubble bubble-one" }
                span { class: "bubble bubble-two" }
                span { class: "bubble bubble-three" }
                span { class: "bubble bubble-four" }
                span { class: "bubble bubble-five" }
            }
            div { class: "water-motes",
                i {}
                i {}
                i {}
                i {}
                i {}
                i {}
            }
        }
    }
}

#[component]
fn RememberingIndicator() -> Element {
    rsx! {
        span { class: "remembering-icon", aria_hidden: "true",
            svg {
                width: "20", height: "20", view_box: "0 0 24 24",
                fill: "none", stroke: "currentColor", stroke_width: "1.6",
                stroke_linecap: "round", stroke_linejoin: "round",
                path { d: "M12 6.5C9.8 5.1 6.8 4.6 3 5.1v13.2c3.8-.5 6.8 0 9 1.4 2.2-1.4 5.2-1.9 9-1.4V5.1c-3.8-.5-6.8 0-9 1.4Zm0 0v13.2" }
                path { class: "remembering-page",
                    d: "M12 6.5c2.5-1.8 4.1-2.3 6.1-2.3v12.9c-2 0-3.6.6-6.1 2.6",
                }
            }
        }
        span { class: "remembering-label", "Remembering…" }
    }
}

#[component]
fn Avatar(name: String, #[props(default = 46)] size: u16) -> Element {
    let initial = name
        .chars()
        .next()
        .unwrap_or('C')
        .to_uppercase()
        .to_string();
    rsx! {
        div { class: "avatar", style: "width: {size}px; height: {size}px", "{initial}" }
    }
}

#[component]
fn Icon(kind: IconKind, #[props(default = 24)] size: u16) -> Element {
    let path = match kind {
        IconKind::Menu => "M4 7h16M4 12h16M4 17h16",
        IconKind::Close => "M6 6l12 12M18 6 6 18",
        IconKind::Chat => "M5 5h14v11H9l-4 3zM8 9h8M8 12h6",
        IconKind::Phone => "M7 4 5 6c0 7 6 13 13 13l2-2-4-4-2 2c-3-1-5-3-6-6l2-2z",
        IconKind::User => "M12 12a4 4 0 1 0 0-8 4 4 0 0 0 0 8zM5 21v-2a7 7 0 0 1 14 0v2",
        IconKind::Contacts => {
            "M9 11a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM3.5 20v-1.5a5.5 5.5 0 0 1 11 0V20M18 7v6M15 10h6"
        }
        IconKind::Plus => "M12 5v14M5 12h14",
        IconKind::ArrowUp => "M12 19V5M6 11l6-6 6 6",
        IconKind::ArrowRight => "M5 12h14M13 6l6 6-6 6",
        IconKind::Clock => "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18zM12 7v5l3 2",
        IconKind::File => "M7 3h7l4 4v14H7zM14 3v5h4M10 13h5M10 17h5",
        IconKind::Logout => "M10 5H5v14h5M14 8l4 4-4 4M18 12H9",
        IconKind::Location => {
            "M12 21s6-5 6-11a6 6 0 1 0-12 0c0 6 6 11 6 11zM12 12a2 2 0 1 0 0-4 2 2 0 0 0 0 4z"
        }
        IconKind::Search => "M11 18a7 7 0 1 0 0-14 7 7 0 0 0 0 14zM16 16l5 5",
        IconKind::ChevronLeft => "M15 5 8 12l7 7",
        IconKind::Headphones => {
            "M4 14v-2a8 8 0 0 1 16 0v2M4 14h3v6H5a1 1 0 0 1-1-1zM20 14h-3v6h2a1 1 0 0 0 1-1z"
        }
        IconKind::Stop => "M7 7h10v10H7z",
    };
    rsx! {
        svg {
            class: "icon",
            width: "{size}",
            height: "{size}",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "1.8",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            path { d: "{path}" }
        }
    }
}

fn start_live_listen(call_id: Uuid) -> Result<(), String> {
    listen::start(call_id)
}

fn stop_live_listen(call_id: Uuid) {
    listen::stop(Some(call_id));
}

fn sign_out(session_state: &mut Signal<Option<Session>>) {
    api::set_token(None);
    session::clear();
    session_state.set(None);
}

fn status_label(status: CallStatus) -> &'static str {
    match status {
        CallStatus::Queued => "Queued",
        CallStatus::Ringing => "Ringing",
        CallStatus::InProgress => "On call",
        CallStatus::Completed => "Completed",
        CallStatus::Failed => "Failed",
    }
}

#[cfg(test)]
mod takeover_tests {
    use super::*;

    #[test]
    fn question_countdown_uses_the_original_deadline_and_stops_at_zero() {
        let started = Utc::now();
        let deadline = started + chrono::Duration::seconds(20);
        assert_eq!(question_seconds_remaining(deadline, started), 20);
        assert_eq!(
            question_seconds_remaining(deadline, started + chrono::Duration::seconds(10)),
            10
        );
        assert_eq!(
            question_seconds_remaining(deadline, deadline - chrono::Duration::milliseconds(1)),
            1
        );
        assert_eq!(
            question_seconds_remaining(deadline, deadline + chrono::Duration::seconds(4)),
            0
        );
    }

    fn snapshot(id: Uuid, status: CallStatus, mode: CallMode) -> CallDetailResponse {
        CallDetailResponse {
            call: Call {
                id,
                direction: shared::CallDirection::Outbound,
                peer_number: "+14155550100".into(),
                task_prompt: None,
                ai_backend: shared::CallAiBackend::default(),
                status,
                started_at: None,
                ended_at: None,
                summary: None,
                connected_seconds: 0,
                created_at: Utc::now(),
            },
            transcript: vec![],
            mode,
            control_message: "Your takeover phone is ringing.".into(),
            chat_id: None,
            handoff: None,
            pending_question: None,
            remembering: false,
        }
    }

    #[test]
    fn status_recovery_keeps_unpersisted_live_transcript() {
        let id = Uuid::new_v4();
        let mut feed = LiveCallTranscript::new(id, CallStatus::Ringing);
        feed.push_delta(Speaker::Callee, "Still talking".into(), 0, 1000);
        feed.refresh(snapshot(
            id,
            CallStatus::InProgress,
            CallMode::TakeoverPending,
        ));
        assert_eq!(feed.status, CallStatus::InProgress);
        assert_eq!(feed.mode, CallMode::TakeoverPending);
        assert!(feed.control_message.contains("ringing"));
        assert_eq!(feed.lines[0].text, "Still talking");
    }

    #[test]
    fn old_call_snapshots_cannot_replace_the_current_call() {
        let id = Uuid::new_v4();
        let mut feed = LiveCallTranscript::new(id, CallStatus::InProgress);
        let before = feed.clone();
        feed.refresh(snapshot(
            Uuid::new_v4(),
            CallStatus::Completed,
            CallMode::Takeover,
        ));
        assert_eq!(feed, before);
    }

    #[test]
    fn final_snapshot_restores_persisted_conversation() {
        let id = Uuid::new_v4();
        let mut feed = LiveCallTranscript::new(id, CallStatus::InProgress);
        let mut detail = snapshot(id, CallStatus::Completed, CallMode::Assistant);
        detail.transcript.push(shared::TranscriptSegment {
            id: Uuid::new_v4(),
            call_id: id,
            speaker: Speaker::User,
            text: "Joined by phone".into(),
            at: Utc::now(),
        });
        feed.refresh(detail);
        assert_eq!(feed.status, CallStatus::Completed);
        assert_eq!(feed.lines[0].speaker, Speaker::User);
        assert_eq!(feed.lines[0].text, "Joined by phone");
    }
}
