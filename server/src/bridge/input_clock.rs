//! One 20ms frame per tick, including during carrier gaps and buffered replay.
//! A burst of queued phone packets must never become a burst to GPT-Live.
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use std::collections::VecDeque;

pub const FRAME_SAMPLES: usize = 160;
const MAX_QUEUE: usize = 8_000 * 5;

pub fn paced_timer() -> tokio::time::Interval {
    let mut timer = tokio::time::interval(std::time::Duration::from_millis(20));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    timer.reset_after(std::time::Duration::from_millis(20));
    timer
}

#[derive(Default)]
pub struct InputClock {
    samples: VecDeque<u8>,
}
impl InputClock {
    pub fn push(&mut self, payload: &str) {
        if let Ok(samples) = BASE64.decode(payload) {
            self.push_samples(samples);
        }
    }
    pub fn push_samples(&mut self, samples: impl IntoIterator<Item = u8>) {
        self.samples.extend(samples);
        let excess = self.samples.len().saturating_sub(MAX_QUEUE);
        self.samples.drain(..excess);
    }
    pub fn clear(&mut self) {
        self.samples.clear();
    }
    pub fn queued_ms(&self) -> usize {
        self.samples.len() / 8
    }
    pub fn frame(&mut self) -> String {
        let mut frame = [0xff; FRAME_SAMPLES];
        for sample in &mut frame {
            if let Some(next) = self.samples.pop_front() {
                *sample = next;
            } else {
                break;
            }
        }
        BASE64.encode(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn sending_work_does_not_accumulate_audio_clock_delay() {
        let mut timer = paced_timer();
        let started = tokio::time::Instant::now();
        for _ in 0..40 {
            timer.tick().await;
            // Socket serialization and sending must fit within the cadence,
            // rather than being added to the next frame's 20ms delay.
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_millis(900),
            "audio clock accumulated processing delay: {:?}",
            started.elapsed()
        );
    }
    #[test]
    fn carrier_bursts_are_bounded_and_drained_as_twenty_millisecond_frames() {
        let mut clock = InputClock::default();
        clock.push_samples([0x22; 8_000 * 9]);
        for _ in 0..250 {
            assert_eq!(BASE64.decode(clock.frame()).unwrap(), [0x22; 160]);
        }
        assert_eq!(BASE64.decode(clock.frame()).unwrap(), [0xff; 160]);
        clock.push_samples([0x33; 80]);
        let frame = BASE64.decode(clock.frame()).unwrap();
        assert_eq!(&frame[..80], &[0x33; 80]);
        assert_eq!(&frame[80..], &[0xff; 80]);
    }
}
