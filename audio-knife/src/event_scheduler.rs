//! An event scheduler.
//!
//! We need to delay events depending on how fast FreeSWITCH plays back the audio frames. This is
//! done by simulating the timing of the playback frames here.
//!
//! The audio requests are immediately forwarded as long there are not 5 seconds of audio playback
//! pending. The other events are delayed until audio is _assumed_ to be played back by FreeSWITCH.
use std::{
    cmp::max,
    collections::VecDeque,
    time::{Duration, Instant},
};

use anyhow::Result;
use tokio::{
    select,
    sync::mpsc::{Receiver, Sender},
    time::sleep,
};
use tracing::debug;

use crate::DEFAULT_FORMAT;
use context_switch::ServerEvent;

#[derive(Debug)]
pub struct EventScheduler {
    /// The Timestamp audio playback is finished.
    audio_finished: Instant,
    /// All pending events.
    pending_events: VecDeque<ServerEvent>,
}

const MAX_BUFFERED_AUDIO: Duration = Duration::from_secs(5);
const WAKEUP_DELAY_WHEN_BUFFERS_ARE_FULL: Duration = Duration::from_secs(1);

impl EventScheduler {
    pub fn new() -> Self {
        Self {
            audio_finished: Instant::now(),
            pending_events: VecDeque::new(),
        }
    }

    pub fn schedule_event(&mut self, event: ServerEvent) {
        self.pending_events.push_back(event);
    }

    pub fn process(
        &mut self,
        now: Instant,
        sender: &Sender<ServerEvent>,
    ) -> Result<Option<Duration>> {
        // Be sure audio_finished is not in the past.
        self.audio_finished = max(now, self.audio_finished);

        loop {
            let Some(next_event) = self.pending_events.front() else {
                return Ok(None);
            };
            match classify(next_event) {
                EventKind::Audio(duration) => {
                    if self.audio_finished >= (now + MAX_BUFFERED_AUDIO) {
                        // Audio buffers are full, process again in a second.
                        return Ok(Some(WAKEUP_DELAY_WHEN_BUFFERS_ARE_FULL));
                    }
                    self.audio_finished += duration;
                }
                EventKind::ControlOrOther => {
                    if now < self.audio_finished {
                        // Some audio is pending, call me again if it's played back.
                        return Ok(Some(self.audio_finished - now));
                    }
                }
            }
            sender.try_send(self.pending_events.pop_front().unwrap())?;
        }
    }
}

#[derive(Debug)]
enum EventKind {
    ControlOrOther,
    Audio(Duration),
}

fn classify(event: &ServerEvent) -> EventKind {
    match event {
        ServerEvent::Audio { samples, .. } => EventKind::Audio(samples_duration(samples.samples())),
        _ => EventKind::ControlOrOther,
    }
}

/// Calculates the duration of audio playback from a vector of audio samples
/// based on the DEFAULT_FORMAT
pub fn samples_duration(samples: &[i16]) -> Duration {
    let duration_secs = samples.len() as f64 / DEFAULT_FORMAT.sample_rate as f64;
    Duration::from_secs_f64(duration_secs)
}

/// Runs an event scheduler that manages the timing of events sent to FreeSWITCH.
///
/// This delays audio pakets if more than 5 seconds are pending, and control pakets if currently
/// audio is being assumed to be played back.
pub async fn event_scheduler(
    mut receiver: Receiver<ServerEvent>,
    sender: Sender<ServerEvent>,
) -> Result<()> {
    let mut scheduler = EventScheduler::new();

    loop {
        let now = Instant::now();

        let mut wakeup_delay = Duration::MAX;
        if let Some(wakeup) = scheduler.process(now, &sender)? {
            wakeup_delay = wakeup;
        }

        select! {
            event = receiver.recv() => {
                match event {
                    Some(event) => {
                        scheduler.schedule_event(event);
                    },
                    None => {
                        // Channel closed, exit
                        debug!("Event receiver channel closed, exiting scheduler");
                        return Ok(());
                    }
                }
            },
            _ = sleep(wakeup_delay) => {
            }
        }
    }
}
