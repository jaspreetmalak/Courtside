//! Phone-call deadlines use actual audio activity, not transcript packet gaps.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use std::time::Duration;
use tokio::time::Instant;

const SPEECH_GAP: Duration = Duration::from_millis(500);
const GOODBYE_START_WAIT: Duration = Duration::from_secs(2);
const MAX_GOODBYE_WAIT: Duration = Duration::from_secs(6);
const PLAYBACK_WAIT: Duration = Duration::from_secs(5);
const SILENCE_WAIT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EndCallState {
    Active,
    Closing,
    WaitingForPlayback,
}

pub(super) struct CallEnding {
    pub state: EndCallState,
    goodbye_deadline: Option<Instant>,
}

impl CallEnding {
    pub fn new() -> Self {
        Self {
            state: EndCallState::Active,
            goodbye_deadline: None,
        }
    }

    /// Returns the generation deadline only on the first request to end.
    pub fn begin(&mut self, now: Instant, request_goodbye: bool) -> Option<Instant> {
        if self.state != EndCallState::Active {
            return None;
        }
        self.state = EndCallState::Closing;
        self.goodbye_deadline = Some(now + MAX_GOODBYE_WAIT);
        Some(
            now + if request_goodbye {
                GOODBYE_START_WAIT
            } else {
                SPEECH_GAP
            },
        )
    }

    /// Silent output must not keep postponing hangup. Continuous speech also
    /// has a fixed limit, even when the model ignores the closing instruction.
    pub fn speech(&self, now: Instant) -> Option<Instant> {
        (self.state == EndCallState::Closing)
            .then(|| (now + SPEECH_GAP).min(self.goodbye_deadline.expect("closing deadline")))
    }

    pub fn wait_for_playback(&mut self, now: Instant) -> Instant {
        self.state = EndCallState::WaitingForPlayback;
        now + PLAYBACK_WAIT
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SilenceAction {
    CheckIn,
    End,
}

pub(super) struct CallSilence {
    last_activity: Instant,
    checked_in: bool,
    // Outbound audio can arrive faster than Twilio plays it. Count the queue
    // as activity so a caller gets a full response window after playback.
    playback_until: Instant,
}

impl CallSilence {
    pub fn new(now: Instant) -> Self {
        Self {
            last_activity: now,
            checked_in: false,
            playback_until: now,
        }
    }

    pub fn recipient_activity(&mut self, now: Instant) {
        self.last_activity = now.max(self.playback_until);
        self.checked_in = false;
    }

    pub fn assistant_audio(&mut self, now: Instant, payload: &str) -> bool {
        let Ok(audio) = BASE64.decode(payload) else {
            return false;
        };
        let audible = pcmu_is_audible(&audio);
        let duration = Duration::from_secs_f64(audio.len() as f64 / 8_000.0);
        // Only audible packets extend the queue estimate. Silence arriving
        // continuously from GPT-Live is not ongoing assistant speech.
        if audible {
            self.playback_until = self.playback_until.max(now) + duration;
            self.last_activity = self.playback_until;
        }
        audible
    }

    pub fn reset(&mut self, now: Instant) {
        *self = Self::new(now);
    }

    pub fn check(&mut self, now: Instant, pending_work: bool) -> Option<SilenceAction> {
        if pending_work {
            self.last_activity = now.max(self.playback_until);
            self.checked_in = false;
            return None;
        }
        if now.saturating_duration_since(self.last_activity) < SILENCE_WAIT {
            return None;
        }
        self.last_activity = now;
        if self.checked_in {
            Some(SilenceAction::End)
        } else {
            self.checked_in = true;
            Some(SilenceAction::CheckIn)
        }
    }
}

pub(super) fn audio_is_audible(payload: &str) -> bool {
    BASE64
        .decode(payload)
        .is_ok_and(|audio| pcmu_is_audible(&audio))
}

/// The phone line is already quiet without a media packet. Forwarding idle
/// frames can queue silence ahead of a subsequent spoken response on Twilio.
pub(super) fn audio_is_digital_silence(payload: &str) -> bool {
    BASE64
        .decode(payload)
        .is_ok_and(|audio| audio.iter().all(|byte| matches!(byte, 0xff | 0x7f)))
}

fn pcmu_is_audible(audio: &[u8]) -> bool {
    // Decode G.711 μ-law to measure RMS. Ignore digital silence and quiet
    // line noise; speech or hold music keeps the recipient's line active.
    if audio.is_empty() {
        return false;
    }
    let energy: u64 = audio
        .iter()
        .map(|byte| {
            let value = !byte;
            let magnitude = (((value & 0x0f) as i64 * 8 + 0x84) << ((value >> 4) & 7)) - 0x84;
            (magnitude * magnitude) as u64
        })
        .sum();
    energy / audio.len() as u64 >= 200 * 200
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuous_goodbye_audio_cannot_postpone_hangup_forever() {
        let now = Instant::now();
        let mut ending = CallEnding::new();
        assert_eq!(ending.begin(now, true), Some(now + GOODBYE_START_WAIT));
        for frame in 0..500 {
            let deadline = ending
                .speech(now + Duration::from_millis(frame * 20))
                .unwrap();
            assert!(deadline <= now + MAX_GOODBYE_WAIT);
        }
        assert!(ending.begin(now + Duration::from_secs(3), true).is_none());
        assert_eq!(ending.wait_for_playback(now), now + PLAYBACK_WAIT);
        assert!(ending.speech(now).is_none());
    }

    #[test]
    fn digital_silence_and_line_noise_do_not_count_as_speech() {
        for sample in [0xff, 0x7f, 0xfe, 0x7e] {
            assert!(!audio_is_audible(&BASE64.encode([sample; 160])));
        }
        assert!(audio_is_audible(&BASE64.encode([0x22; 160])));
        assert!(!audio_is_audible("invalid base64"));
    }

    #[test]
    fn only_exact_digital_silence_can_be_omitted_from_phone_playback() {
        assert!(audio_is_digital_silence(&BASE64.encode([0xff, 0x7f])));
        // Keep even quiet nonzero audio, and every packet containing speech.
        for samples in [[0xff, 0xfe], [0x7f, 0x7e], [0xff, 0x22]] {
            assert!(!audio_is_digital_silence(&BASE64.encode(samples)));
        }
        assert!(!audio_is_digital_silence("invalid base64"));
    }

    #[test]
    fn silent_line_gets_one_check_in_and_then_ends() {
        let now = Instant::now();
        let mut silence = CallSilence::new(now);
        let quiet = BASE64.encode([0xff; 800]);
        for seconds in 0..20 {
            assert!(!silence.assistant_audio(now + Duration::from_secs(seconds), &quiet));
            assert_eq!(
                silence.check(now + Duration::from_secs(seconds), false),
                None
            );
        }
        assert_eq!(
            silence.check(now + SILENCE_WAIT, false),
            Some(SilenceAction::CheckIn)
        );
        assert_eq!(
            silence.check(now + SILENCE_WAIT * 2, false),
            Some(SilenceAction::End)
        );
    }

    #[test]
    fn reply_hold_music_and_private_work_preserve_the_call() {
        let now = Instant::now();
        let mut silence = CallSilence::new(now);
        assert_eq!(
            silence.check(now + SILENCE_WAIT, false),
            Some(SilenceAction::CheckIn)
        );
        silence.recipient_activity(now + SILENCE_WAIT + Duration::from_secs(1));
        assert_eq!(silence.check(now + SILENCE_WAIT * 2, false), None);
        assert_eq!(silence.check(now + Duration::from_secs(300), true), None);
        assert_eq!(silence.check(now + Duration::from_secs(301), false), None);
    }

    #[test]
    fn response_window_starts_after_buffered_speech_finishes() {
        let now = Instant::now();
        let mut silence = CallSilence::new(now);
        let voice = BASE64.encode(vec![0x22; 80_000]); // ten seconds queued
        assert!(silence.assistant_audio(now, &voice));
        assert!(silence.assistant_audio(now, &voice));
        assert_eq!(silence.check(now + Duration::from_secs(30), false), None);
        assert_eq!(
            silence.check(now + Duration::from_secs(40), false),
            Some(SilenceAction::CheckIn)
        );
    }
}
