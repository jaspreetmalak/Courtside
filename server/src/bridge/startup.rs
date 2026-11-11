//! A one-time, input-only hello wakes a quiet Live session. The phone output
//! always comes from GPT-Live; this clip never enters the playback relay.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use std::collections::VecDeque;

pub(super) const HELLO_AUDIO: &[u8] = include_bytes!("assets/startup-hello.pcmu");

pub(super) struct StartupHello {
    attempted: bool,
    offset: Option<usize>,
    input_samples: u64,
    synthetic_range: Option<(i64, i64)>,
    transcript_consumed: bool,
}

impl StartupHello {
    pub fn new(resumed: bool) -> Self {
        Self {
            attempted: resumed,
            offset: None,
            input_samples: 0,
            synthetic_range: None,
            transcript_consumed: false,
        }
    }

    pub fn can_start(&self) -> bool {
        !self.attempted
    }

    pub fn start(&mut self) {
        if self.can_start() {
            self.attempted = true;
            self.offset = Some(0);
        }
    }

    pub fn active(&self) -> bool {
        self.offset.is_some()
    }

    /// Real speech or the assistant's own opening always takes precedence.
    pub fn stop(&mut self) {
        self.attempted = true;
        self.offset = None;
    }

    pub fn record_input(&mut self, payload: &str) {
        if let Ok(audio) = BASE64.decode(payload) {
            self.input_samples += audio.len() as u64;
        }
    }

    pub fn next_frame(&mut self) -> Option<&'static [u8]> {
        let offset = self.offset?;
        let frame = &HELLO_AUDIO[offset..(offset + 160).min(HELLO_AUDIO.len())];
        let start_ms = (self.input_samples / 8) as i64;
        self.input_samples += frame.len() as u64;
        let end_ms = (self.input_samples / 8) as i64;
        if let Some(range) = self.synthetic_range.as_mut() {
            range.1 = end_ms;
        } else if super::lifecycle::audio_is_audible(&BASE64.encode(frame)) {
            self.synthetic_range = Some((start_ms, end_ms));
        }
        self.offset = (offset + frame.len() < HELLO_AUDIO.len()).then_some(offset + frame.len());
        Some(frame)
    }

    /// The cue is exactly one word. Hide only its first recognition near the
    /// inserted audio, including delayed captions. Live's timestamps are
    /// approximate and can include the startup context's estimated time.
    /// Other words and later genuine hellos remain part of the transcript.
    pub fn discard_transcript(&mut self, text: &str, start_ms: i64, end_ms: i64) -> bool {
        if self.transcript_consumed {
            return false;
        }
        let Some((start, end)) = self.synthetic_range else {
            return false;
        };
        let word: String = text.chars().filter(|c| c.is_alphabetic()).collect();
        if word.eq_ignore_ascii_case("hello")
            && start_ms >= start.saturating_sub(200)
            && end_ms <= end.saturating_add(1_500)
        {
            self.transcript_consumed = true;
            return true;
        }
        false
    }
}

/// Keep the actual phone reply while the task session connects. This buffer
/// never contains the internal cue and is never used for phone playback.
#[derive(Default)]
pub(super) struct ReplyAudio {
    samples: VecDeque<u8>,
}

impl ReplyAudio {
    pub fn record(&mut self, payload: &str) {
        if let Ok(audio) = BASE64.decode(payload) {
            self.samples.extend(audio);
            let excess = self.samples.len().saturating_sub(8_000 * 16);
            self.samples.drain(..excess);
        }
    }

    pub fn take(&mut self) -> Vec<u8> {
        let audio: Vec<_> = self.samples.drain(..).collect();
        // Retain a little lead-in, without replaying seconds of line silence
        // before the person said hello.
        let first_speech = audio
            .chunks(160)
            .position(|frame| super::lifecycle::audio_is_audible(&BASE64.encode(frame)));
        let start = first_speech
            .map(|frame| (frame * 160).saturating_sub(1_600))
            .unwrap_or_default();
        // The new session already knows the real reply from typed context.
        // Replay its speech, not the seconds of silence spent connecting;
        // otherwise that quiet tail delays all subsequent phone input.
        let end = audio
            .chunks(160)
            .rposition(|frame| super::lifecycle::audio_is_audible(&BASE64.encode(frame)))
            .map(|frame| ((frame + 1) * 160 + 1_600).min(audio.len()))
            .unwrap_or(start);
        audio[start..end].to_vec()
    }
}

/// Replaying the genuine reply gives the new voice session its audio context.
/// Captions already delivered by the presence check must appear only once.
#[derive(Default)]
pub(super) struct ReplayedReply {
    remaining: String,
    through_ms: i64,
}

impl ReplayedReply {
    pub fn new(text: &str, samples: usize) -> Self {
        Self {
            remaining: normalize_caption(text),
            through_ms: (samples / 8) as i64 + 1_500,
        }
    }

    pub fn filter(&mut self, text: &str, end_ms: i64) -> Option<String> {
        if self.remaining.is_empty() {
            return Some(text.into());
        }
        if end_ms > self.through_ms {
            self.remaining.clear();
            return Some(text.into());
        }
        let normalized = normalize_caption(text);
        if normalized.is_empty() {
            return None;
        }
        if self.remaining.starts_with(&normalized) {
            self.remaining.drain(..normalized.len());
            return None;
        }
        if normalized.starts_with(&self.remaining) {
            // A delta can contain both the repeated prefix and new words.
            // Keep the new words rather than dropping the entire delta.
            let mut matched = 0;
            let mut cut = 0;
            for (offset, character) in text.char_indices() {
                if character.is_alphanumeric() {
                    matched += character.to_lowercase().collect::<String>().len();
                    if matched >= self.remaining.len() {
                        cut = offset + character.len_utf8();
                        break;
                    }
                }
            }
            self.remaining.clear();
            let rest = text[cut..].trim_start_matches(|c: char| !c.is_alphanumeric());
            return (!rest.is_empty()).then(|| format!(" {rest}"));
        }
        // Different words are new evidence, even if the model's approximate
        // timestamps overlap the replay window.
        self.remaining.clear();
        Some(text.into())
    }
}

fn normalize_caption(text: &str) -> String {
    text.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cue_is_one_second_of_phone_audio_and_is_inserted_only_once() {
        assert_eq!(HELLO_AUDIO.len() % 160, 0);
        assert!(HELLO_AUDIO.len() <= 8_000);
        assert!(super::super::lifecycle::audio_is_audible(
            &BASE64.encode(HELLO_AUDIO)
        ));
        let mut hello = StartupHello::new(false);
        hello.start();
        let mut frames = Vec::new();
        while let Some(frame) = hello.next_frame() {
            frames.extend_from_slice(frame);
        }
        assert_eq!(frames, HELLO_AUDIO);
        hello.start();
        assert!(hello.next_frame().is_none());
    }

    #[test]
    fn real_speech_and_resumed_calls_prevent_the_cue() {
        let mut hello = StartupHello::new(false);
        hello.stop();
        hello.start();
        assert!(hello.next_frame().is_none());
        let mut resumed = StartupHello::new(true);
        resumed.start();
        assert!(resumed.next_frame().is_none());
    }

    #[test]
    fn only_the_inserted_hello_is_hidden_from_the_transcript() {
        let mut hello = StartupHello::new(false);
        hello.record_input(&BASE64.encode([0xff; 16_000]));
        hello.start();
        while hello.next_frame().is_some() {}
        assert!(!hello.discard_transcript("Yes, I can help.", 3_000, 3_600));
        assert!(hello.discard_transcript("Hello.", 3_200, 3_400));
        assert!(!hello.discard_transcript("Hello.", 3_600, 3_800));
    }

    #[test]
    fn cancellation_before_injection_does_not_hide_a_genuine_hello() {
        let mut hello = StartupHello::new(false);
        hello.start();
        for _ in 0..5 {
            hello.next_frame();
        } // leading silence isn't a spoken cue
        hello.stop();
        assert!(hello.next_frame().is_none());
        assert!(!hello.discard_transcript("Hello.", 0, 200));
    }

    #[test]
    fn replay_keeps_phone_speech_without_a_long_quiet_lead_in() {
        let mut reply = ReplyAudio::default();
        reply.record(&BASE64.encode([0xff; 24_000]));
        reply.record(&BASE64.encode([0x22; 160]));
        let audio = reply.take();
        assert_eq!(audio.len(), 1_760);
        assert_eq!(&audio[1_600..], &[0x22; 160]);
        assert!(reply.take().is_empty());
    }

    #[test]
    fn replay_preserves_additional_words_without_replaying_connection_silence() {
        let mut reply = ReplyAudio::default();
        reply.record(&BASE64.encode([0x22; 160]));
        reply.record(&BASE64.encode([0xff; 8_000]));
        reply.record(&BASE64.encode([0x55; 160]));
        reply.record(&BASE64.encode([0xff; 24_000]));
        let audio = reply.take();
        assert_eq!(audio.len(), 160 + 8_000 + 160 + 1_600);
        assert_eq!(&audio[..160], &[0x22; 160]);
        assert_eq!(&audio[8_160..8_320], &[0x55; 160]);
    }

    #[test]
    fn replay_caption_filter_keeps_new_words_and_later_repeated_greetings() {
        let mut replay = ReplayedReply::new("Hello, this is the restaurant.", 8_000);
        assert!(replay.filter("Hello.", 1_200).is_none());
        assert_eq!(
            replay.filter(" This is the restaurant. How can I help?", 1_600),
            Some(" How can I help?".into())
        );
        assert_eq!(replay.filter("Hello?", 4_000), Some("Hello?".into()));
        let mut replay = ReplayedReply::new("Hello.", 8_000);
        assert_eq!(replay.filter("Yes.", 1_200), Some("Yes.".into()));
        let mut replay = ReplayedReply::new("Hello.", 8_000);
        assert_eq!(replay.filter("Hello.", 3_000), Some("Hello.".into()));
    }
}
