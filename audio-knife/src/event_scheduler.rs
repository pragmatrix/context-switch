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

use anyhow::{Context, Result, bail};
use tokio::{
    select,
    sync::mpsc::{Sender, UnboundedReceiver},
    time::sleep,
};
use tracing::{debug, warn};

use context_switch::{AudioFormat, OutputModality, OutputPath, ServerEvent};

/// Runs an event scheduler that manages the timing of events sent to FreeSWITCH.
///
/// This delays audio pakets if more than 5 seconds are pending, and control pakets if currently
/// audio is being assumed to be played back.
pub async fn event_scheduler(
    mut receiver: UnboundedReceiver<ServerEvent>,
    sender: Sender<ServerEvent>,
) -> Result<()> {
    let mut media_scheduler = MediaEventScheduler::new();

    let mut wakeup_delay = Duration::MAX;
    loop {
        let event = select! {
            event = receiver.recv() => {
                match event {
                    Some(event) => Some(event),
                    None => {
                        // Channel closed, exit
                        debug!("Event receiver channel closed, exiting scheduler");
                        return Ok(());
                    }
                }
            },
            _ = sleep(wakeup_delay) => {
                None
            }
        };

        let now = Instant::now();
        if let Some(event) = event {
            match event.output_path() {
                OutputPath::Control => {
                    if let ServerEvent::Started { modalities, .. } = &event {
                        // TODO: This is ugly here, may be we should set it when we set up the conversation, because
                        // the modalities should be clear from the beginning (no negotiation is currently supported).
                        media_scheduler.notify_started(modalities)?;
                    }
                    // Control path events are sent out immediately.
                    sender.try_send(event).context("Sending control event")?;
                    // Even though only a control event was short circuited we need to kick the the
                    // media scheduler.
                }
                OutputPath::Media => {
                    media_scheduler.schedule_event(now, event);
                }
            }
        }

        if let Some(wakeup) = media_scheduler.process(now, &sender)? {
            wakeup_delay = wakeup;
        } else {
            wakeup_delay = Duration::MAX;
        }
    }
}

#[derive(Debug)]
pub struct MediaEventScheduler {
    /// The Timestamp audio playback is finished.
    audio_finished: Instant,
    /// The input queue of all media path media events.
    input_media_events: VecDeque<ServerEvent>,
    /// The scheudled queue of control events.
    timed_events: VecDeque<(Instant, ServerEvent)>,
    /// Latest audio format seen.
    audio_format: Option<AudioFormat>,
}

const MAX_BUFFERED_AUDIO: Duration = Duration::from_secs(5);
const WAKEUP_DELAY_WHEN_BUFFERS_ARE_FULL: Duration = Duration::from_secs(1);

impl MediaEventScheduler {
    pub fn recommended_channel_capacity(min_audio_frame_duration: Duration) -> usize {
        let max_audio_frames_pending =
            MAX_BUFFERED_AUDIO.as_secs_f64() / min_audio_frame_duration.as_secs_f64();
        // Multiply with 2 to include some safety margin and control packets.
        (max_audio_frames_pending as usize) * 2
    }

    pub fn new() -> Self {
        Self {
            audio_finished: Instant::now(),
            input_media_events: VecDeque::new(),
            timed_events: VecDeque::new(),
            audio_format: None,
        }
    }

    /// TODO: There could be situation in which ... when there is a conversation crossover ... the
    /// started event was not sent yet when we received audio here. In this case, we have to ignore
    /// the audio and warn about it.
    pub fn notify_started(&mut self, modalities: &[OutputModality]) -> Result<()> {
        if self.audio_format.is_some() {
            bail!("Internal error, received output modalities twice.");
        }
        self.audio_format = audio_format_from_output_modalities(modalities)?;
        Ok(())
    }

    pub fn schedule_event(&mut self, now: Instant, event: ServerEvent) {
        // Don't give me anothing other than media path events!
        debug_assert!(event.output_path() == OutputPath::Media);
        if let ServerEvent::ClearAudio { .. } = event {
            self.input_media_events
                .retain(|e| !matches!(e, ServerEvent::Audio { .. }));
            // All the non-audio event before `ClearAudio` must be sent asap, too.
            self.audio_finished = now;
            self.timed_events.iter_mut().for_each(|(t, _)| *t = now);
        }
        self.input_media_events.push_back(event);
    }

    /// Process all the events.
    pub fn process(
        &mut self,
        now: Instant,
        sender: &Sender<ServerEvent>,
    ) -> Result<Option<Duration>> {
        let d1 = self.process_timed_events(now, sender)?;
        let d2 = self.process_media_path_events(now, sender)?;
        match (d1, d2) {
            (Some(d1), Some(d2)) => Ok(Some(d1.min(d2))),
            (Some(d), None) | (None, Some(d)) => Ok(Some(d)),
            (None, None) => Ok(None),
        }
    }

    /// Process all the events of which know when they must be sent.
    pub fn process_timed_events(
        &mut self,
        now: Instant,
        sender: &Sender<ServerEvent>,
    ) -> Result<Option<Duration>> {
        while let Some((time_to_send, _)) = self.timed_events.front()
            && *time_to_send <= now
        {
            let (_, event) = self.timed_events.pop_front().unwrap();
            sender.try_send(event).context("Sending timed event")?;
        }

        Ok(self.timed_events.front().map(|(t, _)| *t - now))
    }

    pub fn process_media_path_events(
        &mut self,
        now: Instant,
        sender: &Sender<ServerEvent>,
    ) -> Result<Option<Duration>> {
        // Be sure audio_finished is not in the past.
        self.audio_finished = max(now, self.audio_finished);

        loop {
            let Some(next_event) = self.input_media_events.front() else {
                return Ok(None);
            };
            match next_event {
                ServerEvent::Audio { samples, .. } => {
                    let Some(audio_format) = self.audio_format else {
                        warn!(
                            "Received Audio but without a prior Started event or no audio output, audio is ignored"
                        );
                        self.input_media_events.pop_front();
                        continue;
                    };
                    let duration = audio_format.duration(samples.len());
                    if self.audio_finished >= (now + MAX_BUFFERED_AUDIO) {
                        // Audio buffers are full, process again later.
                        return Ok(Some(WAKEUP_DELAY_WHEN_BUFFERS_ARE_FULL));
                    }
                    self.audio_finished += duration;

                    sender
                        .try_send(self.input_media_events.pop_front().unwrap())
                        .context("Sending audio event")?;
                }
                _ => {
                    if self.audio_finished > now {
                        // A control event is now in the front, figure out when it's time to send it
                        // out and push it to the list of timed events.
                        let time_to_send = self.audio_finished;
                        let event = self.input_media_events.pop_front().unwrap();
                        self.timed_events.push_back((time_to_send, event));
                        continue;
                    }

                    sender
                        .try_send(self.input_media_events.pop_front().unwrap())
                        .context("Sending control event")?;
                }
            }
        }
    }
}

/// Extract the audio format from output modalities. Returns None or the format. Bails if more than one audio format was found.
fn audio_format_from_output_modalities(
    modalities: &[OutputModality],
) -> Result<Option<AudioFormat>> {
    let mut audio_formats = modalities.iter().filter_map(|modality| match modality {
        OutputModality::Audio { format } => Some(*format),
        _ => None,
    });

    let first_format = audio_formats.next();
    let Some(single_format) = first_format else {
        return Ok(None);
    };

    if audio_formats.next().is_some() {
        bail!("Multiple audio formats found in output modalities");
    }

    Ok(Some(single_format))
}
