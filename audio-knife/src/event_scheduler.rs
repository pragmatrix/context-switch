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

use anyhow::{Result, anyhow, bail};
use tokio::{
    select,
    sync::mpsc::{Receiver, Sender},
    time::sleep,
};
use tracing::debug;

use context_switch::{AudioFormat, OutputModality, ServerEvent};

#[derive(Debug)]
pub struct EventScheduler {
    /// The Timestamp audio playback is finished.
    audio_finished: Instant,
    /// All pending events.
    pending_events: VecDeque<ServerEvent>,
    /// Latest audio format seen.
    audio_format: Option<AudioFormat>,
}

const MAX_BUFFERED_AUDIO: Duration = Duration::from_secs(5);
const WAKEUP_DELAY_WHEN_BUFFERS_ARE_FULL: Duration = Duration::from_secs(1);

impl EventScheduler {
    pub fn new() -> Self {
        Self {
            audio_finished: Instant::now(),
            pending_events: VecDeque::new(),
            audio_format: None,
        }
    }

    pub fn schedule_event(&mut self, now: Instant, event: ServerEvent) {
        if let ServerEvent::ClearAudio { .. } = event {
            self.pending_events
                .retain(|e| !matches!(e, ServerEvent::Audio { .. }));
            // All the non-audio event before `ClearAudio` must be sent asap, too.
            self.audio_finished = now;
        }
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
            match next_event {
                ServerEvent::Started { modalities, .. } => {
                    self.audio_format = Some(audio_format_from_output_modalities(modalities)?);
                }
                ServerEvent::Audio { samples, .. } => {
                    let Some(audio_format) = self.audio_format else {
                        bail!("Received Audio but without a prior Started event")
                    };
                    let duration = audio_format.duration(samples.len());
                    if self.audio_finished >= (now + MAX_BUFFERED_AUDIO) {
                        // Audio buffers are full, process again in a second.
                        return Ok(Some(WAKEUP_DELAY_WHEN_BUFFERS_ARE_FULL));
                    }
                    self.audio_finished += duration;
                }
                _ => {
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

/// Extract the audio format from output modalities. Bails if not exactly one audio format was found.
fn audio_format_from_output_modalities(modalities: &[OutputModality]) -> Result<AudioFormat> {
    let mut formats = modalities.iter().filter_map(|modality| match modality {
        OutputModality::Audio { format } => Some(*format),
        _ => None,
    });

    let first_format = formats
        .next()
        .ok_or_else(|| anyhow!("No audio format found in output modalities"))?;

    if formats.next().is_some() {
        bail!("Multiple audio formats found in output modalities");
    }

    Ok(first_format)
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

    let mut wakeup_delay = Duration::MAX;
    loop {
        let now;
        select! {
            event = receiver.recv() => {
                match event {
                    Some(event) => {
                        now = Instant::now();
                        scheduler.schedule_event(now, event);
                    },
                    None => {
                        // Channel closed, exit
                        debug!("Event receiver channel closed, exiting scheduler");
                        return Ok(());
                    }
                }
            },
            _ = sleep(wakeup_delay) => {
                now = Instant::now()
            }
        }

        if let Some(wakeup) = scheduler.process(now, &sender)? {
            wakeup_delay = wakeup;
        } else {
            wakeup_delay = Duration::MAX;
        }
    }
}
