use crate::domain::{
    classify_midi_message, BasicSettings, Compartment, CompoundMappingSource, ControlEvent,
    ControlEventTimestamp, ControlMainTask, ControlMode, ControlOptions, FeedbackSendBehavior,
    Garbage, GarbageBin, InstanceId, LifecycleMidiMessage, LifecyclePhase, MappingId, MatchOutcome,
    MidiClockCalculator, MidiEvent, MidiMessageClassification, MidiScanResult, MidiScanner,
    MidiSendTarget, NormalRealTimeToMainThreadTask, OrderedMappingMap, OwnedIncomingMidiMessage,
    PartialControlMatch, PersistentMappingProcessingState, QualifiedMappingId,
    RealTimeCompoundMappingTarget, RealTimeControlContext, RealTimeMapping, RealTimeReaperTarget,
    SampleOffset, SendMidiDestination, VirtualSourceValue,
};
use helgoboss_learn::{ControlValue, MidiSourceValue, RawMidiEvent};
use helgoboss_midi::{
    Channel, ControlChange14BitMessage, ControlChange14BitMessageScanner, DataEntryByteOrder,
    ParameterNumberMessage, PollingParameterNumberMessageScanner, RawShortMessage, ShortMessage,
    ShortMessageFactory, ShortMessageType,
};
use reaper_high::{MidiOutputDevice, Reaper};
use reaper_medium::{Hz, MidiInputDeviceId, MidiOutputDeviceId, OnAudioBufferArgs, SendMidiTime};
use slog::{debug, trace};

use crate::base::{Global, NamedChannelSender, SenderToNormalThread, SenderToRealTimeThread};
use assert_no_alloc::permit_alloc;
use enum_map::{enum_map, EnumMap};
use playtime_clip_engine::main::{ClipRecordDestination, VirtualClipRecordAudioInput};
use playtime_clip_engine::rt::supplier::WriteAudioRequest;
use playtime_clip_engine::rt::{AudioBuf, BasicAudioRequestProps, WeakMatrix};
use std::convert::TryInto;
use std::mem;
use std::ptr::null_mut;
use std::time::Duration;
use vst::api::{EventType, Events, SysExEvent};
use vst::host::Host;
use vst::plugin::HostCallback;

const NORMAL_BULK_SIZE: usize = 100;
const FEEDBACK_BULK_SIZE: usize = 100;

#[derive(Debug)]
pub struct RealTimeProcessor {
    instance_id: InstanceId,
    logger: slog::Logger,
    // Synced processing settings
    settings: BasicSettings,
    control_mode: ControlMode,
    mappings: EnumMap<Compartment, OrderedMappingMap<RealTimeMapping>>,
    // State
    control_is_globally_enabled: bool,
    feedback_is_globally_enabled: bool,
    // Inter-thread communication
    normal_task_receiver: crossbeam_channel::Receiver<NormalRealTimeTask>,
    feedback_task_receiver: crossbeam_channel::Receiver<FeedbackRealTimeTask>,
    feedback_task_sender: SenderToRealTimeThread<FeedbackRealTimeTask>,
    normal_main_task_sender: SenderToNormalThread<NormalRealTimeToMainThreadTask>,
    control_main_task_sender: SenderToNormalThread<ControlMainTask>,
    garbage_bin: GarbageBin,
    // Scanners for more complex MIDI message types
    nrpn_scanner: PollingParameterNumberMessageScanner,
    cc_14_bit_scanner: ControlChange14BitMessageScanner,
    // For MIDI capturing
    midi_scanner: MidiScanner,
    // For MIDI timing clock calculations
    midi_clock_calculator: MidiClockCalculator,
    sample_rate: Hz,
    clip_matrix: Option<WeakMatrix>,
    clip_matrix_is_owned: bool,
    clip_record_task: Option<FxInputClipRecordTask>,
}

#[derive(Debug)]
pub struct FxInputClipRecordTask {
    pub input: VirtualClipRecordAudioInput,
    pub destination: ClipRecordDestination,
}

impl RealTimeProcessor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        instance_id: InstanceId,
        parent_logger: &slog::Logger,
        normal_task_receiver: crossbeam_channel::Receiver<NormalRealTimeTask>,
        feedback_task_receiver: crossbeam_channel::Receiver<FeedbackRealTimeTask>,
        feedback_task_sender: SenderToRealTimeThread<FeedbackRealTimeTask>,
        normal_main_task_sender: SenderToNormalThread<NormalRealTimeToMainThreadTask>,
        control_main_task_sender: SenderToNormalThread<ControlMainTask>,
        garbage_bin: GarbageBin,
    ) -> RealTimeProcessor {
        use Compartment::*;
        RealTimeProcessor {
            instance_id,
            logger: parent_logger.new(slog::o!("struct" => "RealTimeProcessor")),
            settings: Default::default(),
            control_mode: ControlMode::Controlling,
            normal_task_receiver,
            feedback_task_receiver,
            feedback_task_sender,
            normal_main_task_sender,
            control_main_task_sender,
            mappings: enum_map! {
                Controller => ordered_map_with_capacity(1000),
                Main => ordered_map_with_capacity(5000),
            },
            nrpn_scanner: PollingParameterNumberMessageScanner::new(Duration::from_millis(1)),
            cc_14_bit_scanner: Default::default(),
            midi_scanner: Default::default(),
            midi_clock_calculator: Default::default(),
            control_is_globally_enabled: false,
            feedback_is_globally_enabled: false,
            garbage_bin,
            sample_rate: Hz::new(1.0),
            clip_matrix: None,
            clip_matrix_is_owned: false,
            clip_record_task: None,
        }
    }

    pub fn process_incoming_midi_from_vst(
        &mut self,
        event: ControlEvent<MidiEvent<IncomingMidiMessage>>,
        is_transport_start: bool,
        host: &HostCallback,
    ) {
        if self.settings.midi_control_input() == MidiControlInput::FxInput {
            // TODO-medium Maybe also filter when transport stopping
            if is_transport_start
                && event
                    .payload()
                    .payload()
                    .might_be_automatically_generated_by_reaper()
            {
                // Ignore note off messages which are a result of starting the transport. They
                // are generated by REAPER in order to stop instruments from sounding. But ReaLearn
                // is not an instrument in the classical sense. We don't want to reset target values
                // just because play has been pressed!
                self.process_unmatched(event.payload(), Caller::Vst(host));
                return;
            }
            self.process_incoming_midi(event, Caller::Vst(host));
        } else {
            // #33, #290 If MIDI input device is not set to <FX input>, we want to pass through all
            // messages that arrive on FX input.
            self.send_incoming_midi_to_fx_output(event.payload(), Caller::Vst(host))
        }
    }

    pub fn run_from_vst(
        &mut self,
        buffer: &mut vst::buffer::AudioBuffer<f64>,
        block_props: AudioBlockProps,
        host: &HostCallback,
    ) {
        self.process_clip_record_task(buffer.split().0, block_props);
        self.process_feedback_tasks(Caller::Vst(host));
    }

    /// This should be regularly called by audio hook in normal mode.
    pub fn run_from_audio_hook_all(
        &mut self,
        block_props: AudioBlockProps,
        might_be_rebirth: bool,
        timestamp: ControlEventTimestamp,
    ) {
        self.run_from_audio_hook_essential(block_props, might_be_rebirth);
        self.run_from_audio_hook_control_and_learn(timestamp);
    }

    pub fn midi_control_input(&self) -> MidiControlInput {
        self.settings.midi_control_input()
    }

    pub fn control_is_globally_enabled(&self) -> bool {
        self.control_is_globally_enabled
    }

    /// This should be called by audio hook in normal mode whenever it receives a MIDI message that
    /// is relevant *for this ReaLearn instance* (the input device is not checked again).
    ///
    /// Returns whether this message should be filtered out from the global MIDI stream.
    pub fn process_incoming_midi_from_audio_hook(
        &mut self,
        event: ControlEvent<MidiEvent<IncomingMidiMessage>>,
    ) -> bool {
        let match_outcome = self.process_incoming_midi(event, Caller::AudioHook);
        let let_through = (match_outcome.matched_or_consumed()
            && self.settings.let_matched_events_through)
            || (!match_outcome.matched_or_consumed() && self.settings.let_unmatched_events_through);
        !let_through
    }

    fn request_full_sync_and_discard_tasks_if_successful(&mut self) {
        if self
            .normal_main_task_sender
            .try_to_send(NormalRealTimeToMainThreadTask::FullResyncToRealTimeProcessorPlease)
        {
            // Requesting a full resync was successful so we can safely discard accumulated tasks.
            let discarded_normal_task_count = self
                .normal_task_receiver
                .try_iter()
                .map(|t| self.garbage_bin.dispose(Garbage::NormalRealTimeTask(t)))
                .count();
            let discarded_feedback_task_count = self
                .feedback_task_receiver
                .try_iter()
                .map(|t| self.garbage_bin.dispose(Garbage::FeedbackRealTimeTask(t)))
                .count();
            permit_alloc(|| {
                debug!(
                    self.logger,
                    "Successfully requested full sync. Discarded {} normal and {} feedback tasks.",
                    discarded_normal_task_count,
                    discarded_feedback_task_count
                );
            });
        } else {
            permit_alloc(|| {
                debug!(
                    self.logger,
                    "Small audio device outage detected but probably related to project load so no action taken.",
                );
            });
        }
    }

    /// This should be regularly called by audio hook even during global MIDI source learning.
    ///
    /// The rebirth parameter is `true` if this could be the first audio cycle after an "unplanned"
    /// downtime of the audio device. It could also be just a downtime related to opening the
    /// project itself, which we detect to some degree. See the code that reacts to this parameter.
    pub fn run_from_audio_hook_essential(
        &mut self,
        block_props: AudioBlockProps,
        might_be_rebirth: bool,
    ) {
        // Poll if this is the clip matrix of this instance. If we would do polling for a foreign
        // clip matrix as well, it would be polled more than once, which is unnecessary.
        if self.clip_matrix_is_owned {
            if let Some(clip_matrix) = self.clip_matrix.as_ref().and_then(|m| m.upgrade()) {
                clip_matrix.lock().poll(block_props.to_playtime());
            }
        }
        // Increase MIDI clock calculator's sample counter
        self.midi_clock_calculator
            .increase_sample_counter_by(block_props.block_length as u64);
        if might_be_rebirth {
            self.request_full_sync_and_discard_tasks_if_successful();
        }
        // Process occasional tasks sent from other thread (probably main thread)
        let normal_task_count = self.normal_task_receiver.len();
        for task in self.normal_task_receiver.try_iter().take(NORMAL_BULK_SIZE) {
            use NormalRealTimeTask::*;
            match task {
                UpdateControlIsGloballyEnabled(is_enabled) => {
                    self.control_is_globally_enabled = is_enabled;
                }
                UpdateFeedbackIsGloballyEnabled(is_enabled) => {
                    // Handle lifecycle MIDI
                    if self.settings.midi_destination().is_some()
                        && is_enabled != self.feedback_is_globally_enabled
                    {
                        self.send_lifecycle_midi_for_all_mappings(is_enabled.into());
                    }
                    // Set
                    self.feedback_is_globally_enabled = is_enabled;
                }
                UpdateAllMappings(compartment, mut mappings) => {
                    permit_alloc(|| {
                        debug!(
                            self.logger,
                            "Updating {} mappings in {}...",
                            mappings.len(),
                            compartment
                        );
                    });
                    // Handle deactivation MIDI
                    if self.processor_feedback_is_effectively_on() {
                        self.send_lifecycle_midi_for_all_mappings_in(
                            compartment,
                            LifecyclePhase::Deactivation,
                        );
                    }
                    // Clear existing mappings (without deallocating)
                    for (_, m) in self.mappings[compartment].drain(..) {
                        self.garbage_bin.dispose_real_time_mapping(m);
                    }
                    // Set
                    let drained_mappings = mappings.drain(..).map(|m| (m.id(), m));
                    self.mappings[compartment].extend(drained_mappings);
                    self.garbage_bin
                        .dispose(Garbage::RealTimeMappings(mappings));
                    // Handle activation MIDI
                    if self.processor_feedback_is_effectively_on() {
                        self.send_lifecycle_midi_for_all_mappings_in(
                            compartment,
                            LifecyclePhase::Activation,
                        );
                    }
                }
                UpdateSingleMapping(compartment, mut mapping) => {
                    let m = std::mem::replace(&mut *mapping, None)
                        .expect("must send a mapping when updating single mapping");
                    self.garbage_bin
                        .dispose(Garbage::BoxedRealTimeMapping(mapping));
                    permit_alloc(|| {
                        debug!(
                            self.logger,
                            "Updating single mapping {:?} in {}...",
                            m.id(),
                            compartment,
                        );
                    });
                    // Send lifecycle MIDI
                    if self.processor_feedback_is_effectively_on() {
                        let was_on_before = self.mappings[compartment]
                            .get(&m.id())
                            .map_or(false, |m| m.feedback_is_effectively_on());
                        let is_on_now = m.feedback_is_effectively_on();
                        self.send_lifecycle_midi_diff(&m, was_on_before, is_on_now)
                    }
                    // Update
                    let old_mapping = self.mappings[compartment].insert(m.id(), m);
                    if let Some(m) = old_mapping {
                        self.garbage_bin.dispose_real_time_mapping(m);
                    }
                }
                UpdatePersistentMappingProcessingState { id, state } => {
                    permit_alloc(|| {
                        debug!(
                            self.logger,
                            "Updating persistent state of {:?} in {}...", id.id, id.compartment
                        );
                    });
                    // Update
                    let (was_on_before, is_on_now) =
                        if let Some(m) = self.mappings[id.compartment].get_mut(&id.id) {
                            let was_on_before = m.feedback_is_effectively_on();
                            m.update_persistent_processing_state(state);
                            (was_on_before, m.feedback_is_effectively_on())
                        } else {
                            (false, false)
                        };
                    // Send lifecycle MIDI
                    if self.processor_feedback_is_effectively_on() {
                        if let Some(m) = self.mappings[id.compartment].get(&id.id) {
                            self.send_lifecycle_midi_diff(m, was_on_before, is_on_now);
                        }
                    }
                }
                UpdateTargetsPartially(compartment, mut target_updates) => {
                    // Also log sample count in order to be sure about invocation order
                    // (timestamp is not accurate enough on e.g. selection changes).
                    // TODO-low We should use an own logger and always log the sample count
                    //  automatically.
                    permit_alloc(|| {
                        debug!(
                            self.logger,
                            "Update target activations in {} at {} samples...",
                            compartment,
                            self.midi_clock_calculator.current_sample_count()
                        );
                    });
                    // Apply updates
                    for update in target_updates.iter_mut() {
                        if let Some(m) = self.mappings[compartment].get_mut(&update.id) {
                            m.update_target(update);
                        }
                    }
                    // Handle lifecycle MIDI
                    if self.processor_feedback_is_effectively_on() {
                        for update in target_updates.iter() {
                            if let Some(activation_change) = update.activation_change {
                                if let Some(m) = self.mappings[compartment].get(&update.id) {
                                    if m.feedback_is_effectively_on_ignoring_target_activation() {
                                        self.send_lifecycle_midi_to_feedback_output_from_audio_hook(
                                            m,
                                            activation_change.is_active.into(),
                                        );
                                    }
                                }
                            }
                        }
                    }
                    self.garbage_bin
                        .dispose(Garbage::TargetUpdates(target_updates));
                }
                UpdateSettings(settings) => {
                    permit_alloc(|| {
                        debug!(self.logger, "Updating settings...");
                    });
                    let prev_midi_destination = self.settings.midi_destination();
                    let next_midi_destination = settings.midi_destination();
                    self.settings = settings;
                    let midi_destination_changing = prev_midi_destination != next_midi_destination;
                    // Handle deactivation
                    if self.processor_feedback_is_effectively_on() && midi_destination_changing {
                        self.send_lifecycle_midi_for_all_mappings(LifecyclePhase::Deactivation);
                    }
                    // Handle activation
                    if self.processor_feedback_is_effectively_on() && midi_destination_changing {
                        self.send_lifecycle_midi_for_all_mappings(LifecyclePhase::Activation);
                    }
                }
                UpdateSampleRate(sample_rate) => {
                    permit_alloc(|| {
                        debug!(self.logger, "Updating sample rate");
                    });
                    self.sample_rate = sample_rate;
                    self.midi_clock_calculator.update_sample_rate(sample_rate);
                }
                StartLearnSource {
                    allow_virtual_sources,
                } => {
                    permit_alloc(|| {
                        debug!(self.logger, "Start learning source");
                    });
                    self.control_mode = ControlMode::LearningSource {
                        allow_virtual_sources,
                        osc_arg_index_hint: None,
                    };
                    self.midi_scanner.reset();
                }
                DisableControl => {
                    permit_alloc(|| {
                        debug!(self.logger, "Disable control");
                    });
                    self.control_mode = ControlMode::Disabled;
                }
                ReturnToControlMode => {
                    permit_alloc(|| {
                        debug!(self.logger, "Return to control mode");
                    });
                    self.control_mode = ControlMode::Controlling;
                    self.nrpn_scanner.reset();
                    self.cc_14_bit_scanner.reset();
                }
                LogDebugInfo => {
                    self.log_debug_info(normal_task_count);
                }
                LogMapping(compartment, mapping_id) => {
                    self.log_mapping(compartment, mapping_id);
                }
                UpdateMappingsPartially(compartment, mapping_updates) => {
                    permit_alloc(|| {
                        debug!(self.logger, "Updating mapping activations...");
                    });
                    // Apply updates
                    for update in mapping_updates.iter() {
                        if let Some(m) = self.mappings[compartment].get_mut(&update.id) {
                            m.update(update);
                        }
                    }
                    // Handle lifecycle MIDI
                    if self.processor_feedback_is_effectively_on() {
                        for update in mapping_updates.iter() {
                            if let Some(m) = self.mappings[compartment].get(&update.id) {
                                if let Some(activation_change) = update.activation_change {
                                    if m.feedback_is_effectively_on_ignoring_mapping_activation() {
                                        self.send_lifecycle_midi_to_feedback_output_from_audio_hook(
                                            m,
                                            activation_change.is_active.into(),
                                        );
                                    }
                                }
                            }
                        }
                    }
                    self.garbage_bin
                        .dispose(Garbage::MappingUpdates(mapping_updates));
                }
                SetClipMatrix { is_owned, matrix } => {
                    self.clip_matrix_is_owned = is_owned;
                    if let Some(matrix) = mem::replace(&mut self.clip_matrix, matrix) {
                        self.garbage_bin.dispose(Garbage::ClipMatrix(matrix));
                    }
                }
                StartClipRecording(task) => {
                    tracing_debug!("Real-time processor received clip record task");
                    self.clip_record_task = Some(task);
                }
            }
        }
    }

    fn process_clip_record_task(
        &mut self,
        inputs: vst::buffer::Inputs<f64>,
        block_props: AudioBlockProps,
    ) {
        if let Some(t) = &mut self.clip_record_task {
            if !process_clip_record_task(t, inputs, block_props.to_playtime()) {
                tracing_debug!("Clearing clip record task from real-time processor");
                self.clip_record_task = None;
            }
        }
    }

    fn send_lifecycle_midi_diff(&self, m: &RealTimeMapping, was_on_before: bool, is_on_now: bool) {
        if is_on_now {
            self.send_lifecycle_midi_to_feedback_output_from_audio_hook(
                m,
                LifecyclePhase::Activation,
            );
        } else if was_on_before {
            self.send_lifecycle_midi_to_feedback_output_from_audio_hook(
                m,
                LifecyclePhase::Deactivation,
            );
        }
    }

    fn processor_feedback_is_effectively_on(&self) -> bool {
        self.feedback_is_globally_enabled && self.settings.midi_destination().is_some()
    }

    fn send_lifecycle_midi_for_all_mappings(&self, phase: LifecyclePhase) {
        for compartment in Compartment::enum_iter() {
            self.send_lifecycle_midi_for_all_mappings_in(compartment, phase);
        }
    }

    fn send_lifecycle_midi_for_all_mappings_in(
        &self,
        compartment: Compartment,
        phase: LifecyclePhase,
    ) {
        for m in self.mappings[compartment].values() {
            if m.feedback_is_effectively_on() {
                self.send_lifecycle_midi_to_feedback_output_from_audio_hook(m, phase);
            }
        }
    }

    /// This should *not* be called by the global audio hook when it's globally capturing MIDI
    /// because we want to pause controlling in that case!
    fn run_from_audio_hook_control_and_learn(&mut self, timestamp: ControlEventTimestamp) {
        match self.control_mode {
            ControlMode::Disabled => {}
            ControlMode::Controlling => {
                // This NRPN scanner is just for controlling, not for learning.
                if self.control_is_globally_enabled {
                    // Poll (N)RPN scanner
                    for ch in 0..16 {
                        if let Some(nrpn_msg) = self.nrpn_scanner.poll(Channel::new(ch)) {
                            // TODO-medium We should memorize the offset of the latest short message
                            //  making up the NRPN message instead!
                            let midi_event = MidiEvent::without_offset(nrpn_msg);
                            let control_event = ControlEvent::new(midi_event, timestamp);
                            self.process_incoming_midi_normal_nrpn(
                                control_event,
                                Caller::AudioHook,
                            );
                        }
                    }
                }
            }
            ControlMode::LearningSource {
                allow_virtual_sources,
                ..
            } => {
                // For local learning/filtering
                if let Some(res) = self.midi_scanner.poll() {
                    self.send_captured_midi(res, allow_virtual_sources);
                }
            }
        }
    }

    fn process_feedback_tasks(&self, caller: Caller) {
        // Process (frequent) feedback tasks sent from other thread (probably main thread)
        for task in self
            .feedback_task_receiver
            .try_iter()
            .take(FEEDBACK_BULK_SIZE)
        {
            use FeedbackRealTimeTask::*;
            match task {
                FxOutputFeedback(v) => {
                    // If the feedback driver is not VST, this will be discarded, no problem.
                    self.send_midi_feedback(v, caller);
                }
                SendLifecycleMidi(compartment, mapping_id, phase) => {
                    if let Some(m) = self.mappings[compartment].get(&mapping_id) {
                        self.send_lifecycle_midi_to_fx_output(
                            m.lifecycle_midi_messages(phase),
                            caller,
                        );
                    }
                }
                NonAllocatingFxOutputFeedback(evt) => {
                    send_raw_midi_to_fx_output(evt.bytes(), SampleOffset::ZERO, caller);
                }
            }
        }
    }

    fn log_debug_info(&self, task_count: usize) {
        // Summary
        permit_alloc(|| {
            let msg = format!(
                "\n\
            # Real-time processor\n\
            \n\
            - Instance ID: {} \n\
            - State: {:?} \n\
            - Total main mapping count: {} \n\
            - Enabled main mapping count: {} \n\
            - Total controller mapping count: {} \n\
            - Enabled controller mapping count: {} \n\
            - Normal task count: {} \n\
            - Feedback task count: {} \n\
            ",
                self.instance_id,
                self.control_mode,
                self.mappings[Compartment::Main].len(),
                self.mappings[Compartment::Main]
                    .values()
                    .filter(|m| m.control_is_effectively_on())
                    .count(),
                self.mappings[Compartment::Controller].len(),
                self.mappings[Compartment::Controller]
                    .values()
                    .filter(|m| m.control_is_effectively_on())
                    .count(),
                task_count,
                self.feedback_task_receiver.len(),
            );
            Global::task_support()
                .do_in_main_thread_asap(move || {
                    Reaper::get().show_console_msg(msg);
                })
                .unwrap();
            // Detailled
            trace!(
                self.logger,
                "\n\
            # Real-time processor\n\
            \n\
            {:#?}
            ",
                self
            );
        });
    }

    fn log_mapping(&self, compartment: Compartment, mapping_id: MappingId) {
        permit_alloc(|| {
            let mapping = self.mappings[compartment].get(&mapping_id);
            let msg = format!(
                "\n\
            # Real-time processor\n\
            \n\
            Mapping with ID {}:\n\
            {:#?}
            ",
                mapping_id, mapping
            );
            Global::task_support()
                .do_in_main_thread_asap(move || {
                    Reaper::get().show_console_msg(msg);
                })
                .unwrap();
        });
    }

    fn process_incoming_midi(
        &mut self,
        event: ControlEvent<MidiEvent<IncomingMidiMessage>>,
        caller: Caller,
    ) -> MatchOutcome {
        use MidiMessageClassification::*;
        match classify_midi_message(event.payload().payload()) {
            Normal => self.process_incoming_midi_normal(event, caller),
            Ignored => {
                // ReaLearn doesn't process those. Forward them if user wants it.
                self.process_unmatched(event.payload(), caller);
                MatchOutcome::Unmatched
            }
            Timing => {
                // Timing clock messages are treated special (calculates BPM).
                // This is control-only, we never learn it.
                if self.control_is_globally_enabled {
                    if let Some(bpm) = self.midi_clock_calculator.feed(event.payload().offset()) {
                        let source_value = MidiSourceValue::<RawShortMessage>::Tempo(bpm);
                        self.control_midi(
                            event.with_payload(MidiEvent::new(
                                event.payload().offset(),
                                &source_value,
                            )),
                            caller,
                        )
                    } else {
                        MatchOutcome::Unmatched
                    }
                } else {
                    MatchOutcome::Unmatched
                }
            }
        }
    }

    /// This basically splits the stream of short MIDI messages into 3 streams:
    ///
    /// - (N)RPN messages
    /// - 14-bit CC messages
    /// - Short MIDI messaages
    fn process_incoming_midi_normal(
        &mut self,
        event: ControlEvent<MidiEvent<IncomingMidiMessage>>,
        caller: Caller,
    ) -> MatchOutcome {
        match self.control_mode {
            ControlMode::Controlling => {
                if self.control_is_globally_enabled {
                    // Even if an composite message ((N)RPN or CC 14-bit) was scanned, we still
                    // process the plain short MIDI message. This is desired.
                    // Rationale: If there's no mapping with a composite source
                    // of this kind, then all the CCs potentially involved in
                    // composite messages can still be used separately (e.g. CC
                    // 6, 38, 98, etc.). That's important! However, if there's
                    // at least one mapping source that listens to composite
                    // messages of the incoming kind, we need to make sure that the
                    // single messages can't be used anymore! Otherwise it would be
                    // confusing. They are consumed. That's the reason why
                    // we do the consumption check at a later state.
                    let plain_match_outcome =
                        self.process_incoming_midi_normal_plain(event, caller);
                    let midi_event = event.payload();
                    let (nrpn_match_outcome, cc14_match_outcome) = match midi_event.payload() {
                        IncomingMidiMessage::Short(short_msg) => {
                            let mut nrpn_match_outcome = MatchOutcome::Unmatched;
                            for nrpn_msg in self.nrpn_scanner.feed(&short_msg).iter().flatten() {
                                let nrpn_event = event
                                    .with_payload(MidiEvent::new(midi_event.offset(), *nrpn_msg));
                                let child_match_outcome =
                                    self.process_incoming_midi_normal_nrpn(nrpn_event, caller);
                                nrpn_match_outcome.upgrade_from(child_match_outcome);
                            }
                            let cc14_match_outcome = if let Some(cc14_msg) =
                                self.cc_14_bit_scanner.feed(&short_msg)
                            {
                                let cc14_event = event
                                    .with_payload(MidiEvent::new(midi_event.offset(), cc14_msg));
                                self.process_incoming_midi_normal_cc14(cc14_event, caller)
                            } else {
                                MatchOutcome::Unmatched
                            };
                            (nrpn_match_outcome, cc14_match_outcome)
                        }
                        // A sys-ex message is never part of a compound message.
                        IncomingMidiMessage::SysEx(_) => {
                            (MatchOutcome::Unmatched, MatchOutcome::Unmatched)
                        }
                    };
                    plain_match_outcome
                        .merge_with(nrpn_match_outcome)
                        .merge_with(cc14_match_outcome)
                } else {
                    MatchOutcome::Unmatched
                }
            }
            ControlMode::LearningSource {
                allow_virtual_sources,
                ..
            } => {
                if self.settings.real_input_logging_enabled {
                    self.log_real_learn_input(event.map_payload(|e| e.payload()));
                }
                let scan_result = match event.payload().payload() {
                    IncomingMidiMessage::Short(short_msg) => {
                        self.midi_scanner.feed_short(short_msg, None)
                    }
                    IncomingMidiMessage::SysEx(bytes) => {
                        // It's okay here to temporarily permit allocation because crackling during
                        // learning is not a showstopper.
                        permit_alloc(|| MidiScanResult::try_from_bytes(bytes, None).ok())
                    }
                };
                if let Some(source) = scan_result {
                    self.send_captured_midi(source, allow_virtual_sources);
                }
                MatchOutcome::Consumed
            }
            ControlMode::Disabled => {
                // "Disabled" means we use this for global learning! We consider this therefore as
                // consumed.
                MatchOutcome::Consumed
            }
        }
    }

    /// Returns whether this message matched.
    fn process_incoming_midi_normal_nrpn(
        &mut self,
        event: ControlEvent<MidiEvent<ParameterNumberMessage>>,
        caller: Caller,
    ) -> MatchOutcome {
        let midi_event = event.payload();
        let source_value =
            MidiSourceValue::<RawShortMessage>::ParameterNumber(midi_event.payload());
        let match_outcome = self.control_midi(
            event.with_payload(MidiEvent::new(midi_event.offset(), &source_value)),
            caller,
        );
        if self.settings.real_input_logging_enabled {
            self.log_real_control_input_internal(event.with_payload(source_value), match_outcome);
        }
        if self.settings.midi_control_input() == MidiControlInput::FxInput
            && ((match_outcome.matched_or_consumed() && self.settings.let_matched_events_through)
                || (!match_outcome.matched_or_consumed()
                    && self.settings.let_unmatched_events_through))
        {
            for m in event
                .payload()
                .payload()
                .to_short_messages::<RawShortMessage>(DataEntryByteOrder::MsbFirst)
                .iter()
                .flatten()
            {
                self.send_short_midi_to_fx_output(
                    MidiEvent::new(event.payload().offset(), *m),
                    caller,
                );
            }
        }
        match_outcome
    }

    /// Might allocate!
    fn log_real_control_input(
        &self,
        evt: ControlEvent<MidiSourceValue<RawShortMessage>>,
        consumed: bool,
        matched: bool,
    ) {
        let match_outcome = if consumed {
            MatchOutcome::Consumed
        } else if matched {
            MatchOutcome::Matched
        } else {
            MatchOutcome::Unmatched
        };
        self.log_real_control_input_internal(evt, match_outcome)
    }

    fn log_real_control_input_internal(
        &self,
        evt: ControlEvent<MidiSourceValue<RawShortMessage>>,
        match_outcome: MatchOutcome,
    ) {
        // It's okay to crackle when logging input.
        let timestamp = evt.timestamp();
        let owned_msg = permit_alloc(|| evt.into_payload().try_into_owned());
        if let Ok(msg) = owned_msg {
            self.control_main_task_sender
                .send_complaining(ControlMainTask::LogRealControlInput {
                    event: ControlEvent::new(msg, timestamp),
                    match_outcome,
                });
        }
    }

    /// Might allocate!
    fn log_real_learn_input(&self, evt: ControlEvent<IncomingMidiMessage>) {
        // It's okay if we crackle when logging input.
        let owned_msg = permit_alloc(|| evt.payload().to_owned());
        self.control_main_task_sender
            .send_complaining(ControlMainTask::LogRealLearnInput {
                event: evt.with_payload(owned_msg),
            });
    }

    /// Might allocate!
    fn log_lifecycle_output(&self, value: MidiSourceValue<RawShortMessage>) {
        // It's okay to crackle when logging input.
        if let Ok(value) = permit_alloc(|| value.try_into_owned()) {
            self.normal_main_task_sender
                .send_complaining(NormalRealTimeToMainThreadTask::LogLifecycleOutput { value });
        }
    }

    fn send_captured_midi(&mut self, scan_result: MidiScanResult, allow_virtual_sources: bool) {
        // If plug-in dropped, the receiver might be gone already because main processor is
        // unregistered synchronously.
        self.normal_main_task_sender
            .send_if_space(NormalRealTimeToMainThreadTask::CaptureMidi {
                scan_result,
                allow_virtual_sources,
            });
    }

    /// Returns whether this message matched.
    fn process_incoming_midi_normal_cc14(
        &mut self,
        event: ControlEvent<MidiEvent<ControlChange14BitMessage>>,
        caller: Caller,
    ) -> MatchOutcome {
        let midi_event = event.payload();
        let source_value =
            MidiSourceValue::<RawShortMessage>::ControlChange14Bit(midi_event.payload());
        let match_outcome = self.control_midi(
            event.with_payload(MidiEvent::new(midi_event.offset(), &source_value)),
            caller,
        );
        if self.settings.real_input_logging_enabled {
            self.log_real_control_input_internal(event.with_payload(source_value), match_outcome);
        }
        if self.settings.midi_control_input() == MidiControlInput::FxInput
            && ((match_outcome.matched_or_consumed() && self.settings.let_matched_events_through)
                || (!match_outcome.matched_or_consumed()
                    && self.settings.let_unmatched_events_through))
        {
            for m in midi_event
                .payload()
                .to_short_messages::<RawShortMessage>()
                .iter()
            {
                let short_event = MidiEvent::new(midi_event.offset(), *m);
                self.send_short_midi_to_fx_output(short_event, caller);
            }
        }
        match_outcome
    }

    fn process_incoming_midi_normal_plain(
        &mut self,
        event: ControlEvent<MidiEvent<IncomingMidiMessage>>,
        caller: Caller,
    ) -> MatchOutcome {
        let midi_event = event.payload();
        let source_value = midi_event.payload().to_source_value();
        if self.is_consumed_by_at_least_one_source(midi_event.payload()) {
            if self.settings.real_input_logging_enabled {
                self.log_real_control_input(event.with_payload(source_value), true, false);
            }
            // Some short MIDI messages are just parts of bigger composite MIDI messages,
            // e.g. (N)RPN or 14-bit CCs. If we reach this point, the incoming message
            // could potentially match one of the (N)RPN or 14-bit CC mappings in the list
            // and therefore doesn't qualify anymore as a candidate for normal CC sources.
            return MatchOutcome::Consumed;
        }
        let match_outcome = self.control_midi(
            event.with_payload(MidiEvent::new(midi_event.offset(), &source_value)),
            caller,
        );
        if self.settings.real_input_logging_enabled {
            self.log_real_control_input_internal(event.with_payload(source_value), match_outcome);
        }
        // At this point, we shouldn't have "consumed" anymore because for MIDI sources, no
        // control will be done at all if a message is consumed by at least one mapping (see above).
        if match_outcome.matched_or_consumed() {
            self.process_matched_short(midi_event, caller);
        } else {
            self.process_unmatched(midi_event, caller);
        }
        match_outcome
    }

    fn all_mappings(&self) -> impl Iterator<Item = &RealTimeMapping> {
        Compartment::enum_iter().flat_map(move |compartment| self.mappings[compartment].values())
    }

    fn control_midi(
        &mut self,
        value_event: ControlEvent<MidiEvent<&MidiSourceValue<RawShortMessage>>>,
        caller: Caller,
    ) -> MatchOutcome {
        // We do pattern matching in order to use Rust's borrow splitting.
        let controller_outcome = if let [ref mut controller_mappings, ref mut main_mappings] =
            self.mappings.as_mut_slice()
        {
            control_controller_mappings_midi(
                &self.control_main_task_sender,
                &self.feedback_task_sender,
                controller_mappings,
                main_mappings,
                value_event,
                caller,
                self.settings.midi_destination(),
                self.settings.virtual_input_logging_enabled,
                self.settings.real_output_logging_enabled,
                self.clip_matrix.as_ref(),
            )
        } else {
            unreachable!()
        };
        let main_outcome = self.control_main_mappings_midi(value_event, caller);
        controller_outcome.merge_with(main_outcome)
    }

    fn control_main_mappings_midi(
        &mut self,
        source_value_event: ControlEvent<MidiEvent<&MidiSourceValue<RawShortMessage>>>,
        caller: Caller,
    ) -> MatchOutcome {
        let compartment = Compartment::Main;
        let mut match_outcome = MatchOutcome::Unmatched;
        for m in self.mappings[compartment]
            .values_mut()
            // The UI prevents creating main mappings with virtual targets but a JSON import
            // doesn't. Check again that it's a REAPER target.
            .filter(|m| m.control_is_effectively_on() && m.has_reaper_target())
        {
            if let CompoundMappingSource::Midi(s) = &m.source() {
                let midi_event = source_value_event.payload();
                if let Some(control_value) = s.control(midi_event.payload()) {
                    let _ = process_real_mapping(
                        m,
                        &self.control_main_task_sender,
                        &self.feedback_task_sender,
                        compartment,
                        source_value_event
                            .with_payload(MidiEvent::new(midi_event.offset(), control_value)),
                        ControlOptions {
                            enforce_target_refresh: match_outcome.matched(),
                            ..Default::default()
                        },
                        caller,
                        self.settings.midi_destination(),
                        self.settings.real_output_logging_enabled,
                        self.clip_matrix.as_ref(),
                    );
                    // It can't be consumed because we checked this before for all mappings.
                    match_outcome = MatchOutcome::Matched;
                }
            }
        }
        match_outcome
    }

    fn process_matched_short(&self, event: MidiEvent<IncomingMidiMessage>, caller: Caller) {
        if self.settings.midi_control_input() != MidiControlInput::FxInput {
            return;
        }
        if !self.settings.let_matched_events_through {
            return;
        }
        self.send_incoming_midi_to_fx_output(event, caller);
    }

    fn process_unmatched(&self, event: MidiEvent<IncomingMidiMessage>, caller: Caller) {
        if self.settings.midi_control_input() != MidiControlInput::FxInput {
            return;
        }
        if !self.settings.let_unmatched_events_through {
            return;
        }
        self.send_incoming_midi_to_fx_output(event, caller);
    }

    fn is_consumed_by_at_least_one_source(&self, msg: IncomingMidiMessage) -> bool {
        use IncomingMidiMessage::*;
        match msg {
            Short(msg) => self
                .all_mappings()
                .any(|m| m.control_is_effectively_on() && m.consumes(msg)),
            // Sys-ex is never part of a compound message.
            SysEx(_) => false,
        }
    }

    fn send_midi_feedback(&self, value: MidiSourceValue<RawShortMessage>, caller: Caller) {
        if let Some(evts) = value.to_raw() {
            // TODO-medium We can implement in a way so we only need one host.process_events() call.
            for evt in evts {
                send_raw_midi_to_fx_output(evt.bytes(), SampleOffset::ZERO, caller);
            }
        } else {
            let shorts = value.to_short_messages(DataEntryByteOrder::MsbFirst);
            if shorts[0].is_none() {
                return;
            }
            for short in shorts.iter().flatten() {
                self.send_short_midi_to_fx_output(MidiEvent::without_offset(*short), caller);
            }
        }
        if let Some(garbage) = value.into_garbage() {
            self.garbage_bin.dispose(Garbage::RawMidiEvents(garbage));
        }
    }

    fn send_lifecycle_midi_to_feedback_output_from_audio_hook(
        &self,
        m: &RealTimeMapping,
        phase: LifecyclePhase,
    ) {
        if let Some(output) = self.settings.midi_destination() {
            match output {
                MidiDestination::FxOutput => {
                    // We can't send it now because we don't have safe access to the host callback
                    // because this method is being called from the audio hook.
                    self.feedback_task_sender.send_if_space(
                        FeedbackRealTimeTask::SendLifecycleMidi(m.compartment(), m.id(), phase),
                    );
                }
                MidiDestination::Device(dev_id) => {
                    MidiOutputDevice::new(dev_id).with_midi_output(|mo| {
                        if let Some(mo) = mo {
                            for m in m.lifecycle_midi_messages(phase) {
                                match m {
                                    LifecycleMidiMessage::Short(msg) => {
                                        if self.settings.real_output_logging_enabled {
                                            self.log_lifecycle_output(MidiSourceValue::Plain(*msg));
                                        }
                                        mo.send(*msg, SendMidiTime::Instantly);
                                    }
                                    LifecycleMidiMessage::Raw(data) => {
                                        if self.settings.real_output_logging_enabled {
                                            permit_alloc(|| {
                                                // We don't use this as feedback value,
                                                // at least not in the sense that it
                                                // participates in feedback relay.
                                                let feedback_address_info = None;
                                                let value = MidiSourceValue::single_raw(
                                                    feedback_address_info,
                                                    *data.clone(),
                                                );
                                                self.log_lifecycle_output(value);
                                            });
                                        }
                                        mo.send_msg(&**data, SendMidiTime::Instantly);
                                    }
                                }
                            }
                        }
                    });
                }
            };
        }
    }

    fn send_lifecycle_midi_to_fx_output(&self, messages: &[LifecycleMidiMessage], caller: Caller) {
        for m in messages {
            match m {
                LifecycleMidiMessage::Short(msg) => {
                    if self.settings.real_output_logging_enabled {
                        self.log_lifecycle_output(MidiSourceValue::Plain(*msg));
                    }
                    self.send_short_midi_to_fx_output(MidiEvent::without_offset(*msg), caller)
                }
                LifecycleMidiMessage::Raw(data) => {
                    if self.settings.real_output_logging_enabled {
                        permit_alloc(|| {
                            // We don't use this as feedback value,
                            // at least not in the sense that it
                            // participates in feedback relay.
                            let feedback_address_info = None;
                            let value =
                                MidiSourceValue::single_raw(feedback_address_info, *data.clone());
                            self.log_lifecycle_output(value);
                        });
                    }
                    send_raw_midi_to_fx_output(data.bytes(), SampleOffset::ZERO, caller)
                }
            }
        }
    }

    fn send_incoming_midi_to_fx_output(
        &self,
        event: MidiEvent<IncomingMidiMessage>,
        caller: Caller,
    ) {
        match event.payload() {
            IncomingMidiMessage::Short(s) => {
                self.send_short_midi_to_fx_output(MidiEvent::new(event.offset(), s), caller);
            }
            IncomingMidiMessage::SysEx(s) => send_raw_midi_to_fx_output(s, event.offset(), caller),
        }
    }

    fn send_short_midi_to_fx_output(&self, event: MidiEvent<RawShortMessage>, caller: Caller) {
        let host = match caller {
            Caller::Vst(h) => h,
            _ => {
                // We must not forward MIDI to VST output if this was called from the global audio
                // hook. First, it could lead to strange effects because
                // `HostCallback::process_events()` is supposed to be called only
                // from the VST processing method. Second, it could even lead to a
                // crash because the real-time processor is removed from
                // the audio hook *after* the plug-in has been already unregistered, and then
                // invoking the host callback (in particular dereferencing the
                // AEffect) would be illegal. This is just a last safety check.
                // Processing should stop before even calling this method.
                return;
            }
        };
        let vst_event = build_short_midi_vst_event(event);
        let vst_events = build_vst_events(&vst_event as *const _ as _);
        host.process_events(&vst_events);
    }
}

fn build_vst_events(event: *mut vst::api::Event) -> Events {
    Events {
        num_events: 1,
        _reserved: 0,
        events: [event, null_mut()],
    }
}

fn build_sysex_midi_vst_event(bytes: &[u8], offset: SampleOffset) -> SysExEvent {
    SysExEvent {
        event_type: EventType::SysEx,
        byte_size: std::mem::size_of::<SysExEvent>() as _,
        delta_frames: offset.get() as _,
        _flags: 0,
        data_size: bytes.len() as _,
        _reserved1: 0,
        system_data: bytes.as_ptr() as _,
        _reserved2: 0,
    }
}

fn build_short_midi_vst_event(event: MidiEvent<RawShortMessage>) -> vst::api::MidiEvent {
    let bytes = event.payload().to_bytes();
    vst::api::MidiEvent {
        event_type: EventType::Midi,
        byte_size: std::mem::size_of::<vst::api::MidiEvent>() as _,
        delta_frames: event.offset().get() as _,
        flags: vst::api::MidiEventFlags::REALTIME_EVENT.bits(),
        note_length: 0,
        note_offset: 0,
        midi_data: [bytes.0, bytes.1.get(), bytes.2.get()],
        _midi_reserved: 0,
        detune: 0,
        note_off_velocity: 0,
        _reserved1: 0,
        _reserved2: 0,
    }
}

#[derive(Copy, Clone)]
pub enum Caller<'a> {
    Vst(&'a HostCallback),
    AudioHook,
}

impl<'a> Caller<'a> {
    pub fn is_vst(&self) -> bool {
        matches!(self, Self::Vst(_))
    }
}

/// A task which is sent from time to time.
#[derive(Debug)]
pub enum NormalRealTimeTask {
    SetClipMatrix {
        is_owned: bool,
        matrix: Option<WeakMatrix>,
    },
    UpdateAllMappings(Compartment, Vec<RealTimeMapping>),
    UpdateSingleMapping(Compartment, Box<Option<RealTimeMapping>>),
    UpdatePersistentMappingProcessingState {
        id: QualifiedMappingId,
        state: PersistentMappingProcessingState,
    },
    UpdateSettings(BasicSettings),
    /// This takes care of propagating target activation states and/or real-time target updates
    /// (for non-virtual mappings).
    UpdateTargetsPartially(Compartment, Vec<RealTimeTargetUpdate>),
    /// Updates the activation state of multiple mappings.
    ///
    /// The given vector contains updates just for affected mappings. This is because when a
    /// parameter update occurs we can determine in a very granular way which targets are affected.
    UpdateMappingsPartially(Compartment, Vec<RealTimeMappingUpdate>),
    LogDebugInfo,
    LogMapping(Compartment, MappingId),
    UpdateSampleRate(Hz),
    StartLearnSource {
        allow_virtual_sources: bool,
    },
    DisableControl,
    ReturnToControlMode,
    UpdateControlIsGloballyEnabled(bool),
    UpdateFeedbackIsGloballyEnabled(bool),
    StartClipRecording(FxInputClipRecordTask),
}

#[derive(Copy, Clone, Debug)]
pub struct MappingActivationEffect {
    pub id: MappingId,
    pub active_1_effect: Option<bool>,
    pub active_2_effect: Option<bool>,
}

impl MappingActivationEffect {
    pub fn new(
        id: MappingId,
        active_1_effect: Option<bool>,
        active_2_effect: Option<bool>,
    ) -> Option<MappingActivationEffect> {
        if active_1_effect.is_none() && active_2_effect.is_none() {
            return None;
        }
        let and = MappingActivationEffect {
            id,
            active_1_effect,
            active_2_effect,
        };
        Some(and)
    }
}

/// Depending on the context this can be about mapping activation or target activation.
///
/// It's important that this reflects an actual change, otherwise the real-time processor might
/// send lifecycle MIDI data in the wrong situations.
#[derive(Copy, Clone, Debug)]
pub struct ActivationChange {
    pub is_active: bool,
}

#[derive(Debug)]
pub struct RealTimeTargetUpdate {
    pub id: MappingId,
    pub activation_change: Option<ActivationChange>,
    pub target_change: Option<Option<RealTimeCompoundMappingTarget>>,
}

#[derive(Debug)]
pub struct RealTimeMappingUpdate {
    pub id: MappingId,
    pub activation_change: Option<ActivationChange>,
}

/// A feedback task (which is potentially sent very frequently).
#[derive(Debug)]
pub enum FeedbackRealTimeTask {
    /// When it comes to MIDI feedback, the real-time processor is only responsible for FX output
    /// feedback. Direct-device feedback is taken care of by the global audio hook for reasons of
    /// proper ordering.
    FxOutputFeedback(MidiSourceValue<'static, RawShortMessage>),
    /// If we send raw MIDI events from the "MIDI: Send message" target to "FX output" and the input
    /// is a MIDI device (not FX input), we must very shortly defer sending the message.
    /// Reason: This message arrives from the audio hook. However, we can't forward to FX output
    /// from the audio hook, we must wait until the VST process method is invoked. In order to let
    /// the MIDI event survive, we need to copy it. But we are not allowed to allocate, so the
    /// usual MidiSourceValue Raw variant is not suited.
    NonAllocatingFxOutputFeedback(RawMidiEvent),
    /// Used only if feedback output is <FX output>, otherwise done synchronously.
    SendLifecycleMidi(Compartment, MappingId, LifecyclePhase),
}

impl Drop for RealTimeProcessor {
    fn drop(&mut self) {
        permit_alloc(|| {
            debug!(self.logger, "Dropping real-time processor...");
        });
    }
}

/// MIDI source which provides ReaLearn control data.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum MidiControlInput {
    /// Processes MIDI messages which are fed into ReaLearn FX.
    FxInput,
    /// Processes MIDI messages coming directly from a MIDI input device.
    Device(MidiInputDeviceId),
}

/// MIDI destination to which e.g. ReaLearn's feedback data can be sent.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum MidiDestination {
    /// Routes messages to the ReaLearn FX output.
    FxOutput,
    /// Routes messages directly to a MIDI output device.
    Device(MidiOutputDeviceId),
}

#[allow(clippy::too_many_arguments)]
fn control_controller_mappings_midi(
    main_task_sender: &SenderToNormalThread<ControlMainTask>,
    rt_feedback_sender: &SenderToRealTimeThread<FeedbackRealTimeTask>,
    // Mappings with virtual targets
    controller_mappings: &mut OrderedMappingMap<RealTimeMapping>,
    // Mappings with virtual sources
    main_mappings: &mut OrderedMappingMap<RealTimeMapping>,
    value_event: ControlEvent<MidiEvent<&MidiSourceValue<RawShortMessage>>>,
    caller: Caller,
    midi_feedback_output: Option<MidiDestination>,
    virtual_input_logging_enabled: bool,
    output_logging_enabled: bool,
    matrix: Option<&WeakMatrix>,
) -> MatchOutcome {
    let mut match_outcome = MatchOutcome::Unmatched;
    let mut enforce_target_refresh = false;
    for m in controller_mappings
        .values_mut()
        .filter(|m| m.control_is_effectively_on())
    {
        if let Some(control_match) =
            m.control_midi_virtualizing(flatten_control_midi_event(value_event))
        {
            use PartialControlMatch::*;
            let child_match_outcome = match control_match {
                ProcessVirtual(virtual_source_value) => {
                    let virtual_match_outcome = control_main_mappings_virtual(
                        main_task_sender,
                        rt_feedback_sender,
                        main_mappings,
                        value_event.with_payload(MidiEvent::new(
                            value_event.payload().offset(),
                            virtual_source_value,
                        )),
                        ControlOptions {
                            // We inherit "Send feedback after control" to the main processor if it's
                            // enabled for the virtual mapping. That's the easy way to do it.
                            // Downside: If multiple real control elements are mapped to one virtual
                            // control element, "feedback after control" will be sent to all of those,
                            // which is technically not necessary. It would be enough to just send it
                            // to the one that was touched. However, it also doesn't really hurt.
                            enforce_send_feedback_after_control: m.options().feedback_send_behavior
                                == FeedbackSendBehavior::SendFeedbackAfterControl,
                            mode_control_options: m.mode_control_options(),
                            // Not important yet at this point because virtual targets can't affect
                            // subsequent virtual targets.
                            enforce_target_refresh: false,
                        },
                        caller,
                        midi_feedback_output,
                        output_logging_enabled,
                        matrix,
                    );
                    if virtual_input_logging_enabled {
                        log_virtual_control_input(
                            main_task_sender,
                            value_event.with_payload(virtual_source_value),
                            virtual_match_outcome,
                        );
                    }
                    virtual_match_outcome
                }
                ProcessDirect(control_value) => {
                    let _ = process_real_mapping(
                        m,
                        main_task_sender,
                        rt_feedback_sender,
                        Compartment::Controller,
                        value_event.with_payload(MidiEvent::new(
                            value_event.payload().offset(),
                            control_value,
                        )),
                        ControlOptions {
                            enforce_target_refresh,
                            ..Default::default()
                        },
                        caller,
                        midi_feedback_output,
                        output_logging_enabled,
                        matrix,
                    );
                    // We do this only for transactions of *real* target matches.
                    enforce_target_refresh = true;
                    MatchOutcome::Matched
                }
            };
            match_outcome.upgrade_from(child_match_outcome);
        }
    }
    match_outcome
}

#[allow(clippy::too_many_arguments)]
fn process_real_mapping(
    mapping: &mut RealTimeMapping,
    main_task_sender: &SenderToNormalThread<ControlMainTask>,
    rt_feedback_sender: &SenderToRealTimeThread<FeedbackRealTimeTask>,
    compartment: Compartment,
    value_event: ControlEvent<MidiEvent<ControlValue>>,
    options: ControlOptions,
    caller: Caller,
    midi_feedback_output: Option<MidiDestination>,
    output_logging_enabled: bool,
    clip_matrix: Option<&WeakMatrix>,
) -> Result<(), &'static str> {
    let pure_control_event = flatten_control_midi_event(value_event);
    if let Some(RealTimeCompoundMappingTarget::Reaper(reaper_target)) =
        mapping.resolved_target.as_mut()
    {
        if reaper_target.wants_real_time_control(caller) {
            // Try to process directly here in real-time.
            let control_context = RealTimeControlContext { clip_matrix };
            let control_value: Option<ControlValue> = mapping
                .core
                .mode
                .control_with_options(
                    pure_control_event,
                    reaper_target,
                    control_context,
                    options.mode_control_options,
                    // Performance control not supported when controlling real-time
                    None,
                )
                .ok_or("mode didn't return control value")?
                .into();
            let control_value = control_value.ok_or("target already has desired value")?;
            match reaper_target {
                RealTimeReaperTarget::SendMidi(t) => {
                    real_time_target_send_midi(
                        t,
                        caller,
                        control_value,
                        midi_feedback_output,
                        output_logging_enabled,
                        main_task_sender,
                        rt_feedback_sender,
                        value_event.payload(),
                    )?;
                }
                RealTimeReaperTarget::ClipTransport(t) => {
                    t.hit(control_value, control_context)?;
                }
                RealTimeReaperTarget::ClipColumn(t) => {
                    t.hit(control_value, control_context)?;
                }
                RealTimeReaperTarget::ClipRow(t) => {
                    t.hit(control_value, control_context)?;
                }
                RealTimeReaperTarget::ClipMatrix(t) => {
                    t.hit(control_value, control_context)?;
                }
                RealTimeReaperTarget::FxParameter(t) => {
                    t.hit(control_value)?;
                }
            }
        } else {
            // Forward to main processor.
            forward_control_to_main_processor(
                main_task_sender,
                compartment,
                mapping.id(),
                pure_control_event,
                options,
            );
        }
    } else {
        // Forward to main processor.
        forward_control_to_main_processor(
            main_task_sender,
            compartment,
            mapping.id(),
            pure_control_event,
            options,
        );
    };
    Ok(())
}

// TODO-medium Also keep this more local to SendMidiTarget, just like ClipTransportTarget.
#[allow(clippy::too_many_arguments)]
fn real_time_target_send_midi(
    t: &mut MidiSendTarget,
    caller: Caller,
    control_value: ControlValue,
    midi_feedback_output: Option<MidiDestination>,
    output_logging_enabled: bool,
    main_task_sender: &SenderToNormalThread<ControlMainTask>,
    rt_feedback_sender: &SenderToRealTimeThread<FeedbackRealTimeTask>,
    value_event: MidiEvent<ControlValue>,
) -> Result<(), &'static str> {
    let v = control_value.to_absolute_value()?;
    // This is a type of mapping that we should process right here because we want to
    // send a MIDI message and this needs to happen in the audio thread.
    // Going to the main thread and back would be such a waste!
    let raw_midi_event = t.pattern().to_concrete_midi_event(v);
    let midi_destination = match caller {
        Caller::Vst(_) => match t.destination() {
            SendMidiDestination::FxOutput => Some(MidiDestination::FxOutput),
            SendMidiDestination::FeedbackOutput => {
                Some(midi_feedback_output.ok_or("no feedback output set")?)
            }
        },
        Caller::AudioHook => match t.destination() {
            SendMidiDestination::FxOutput => Some(MidiDestination::FxOutput),
            SendMidiDestination::FeedbackOutput => {
                Some(midi_feedback_output.ok_or("no feedback output set")?)
            }
        },
    };
    if output_logging_enabled && midi_destination.is_some() {
        permit_alloc(|| {
            main_task_sender.send_complaining(ControlMainTask::LogTargetOutput {
                event: Box::new(raw_midi_event),
            });
        });
    }
    let successful = match midi_destination {
        Some(MidiDestination::FxOutput) => {
            match caller {
                Caller::Vst(_) => {
                    send_raw_midi_to_fx_output(
                        raw_midi_event.bytes(),
                        value_event.offset(),
                        caller,
                    );
                }
                Caller::AudioHook => {
                    // We can't send to FX output here directly. Need to wait until VST processing
                    // starts (same processing cycle).
                    rt_feedback_sender.send_complaining(
                        FeedbackRealTimeTask::NonAllocatingFxOutputFeedback(raw_midi_event),
                    );
                }
            }
            true
        }
        Some(MidiDestination::Device(dev_id)) => {
            MidiOutputDevice::new(dev_id).with_midi_output(|mo| {
                if let Some(mo) = mo {
                    mo.send_msg(&raw_midi_event, SendMidiTime::Instantly);
                    true
                } else {
                    false
                }
            })
        }
        _ => false,
    };
    if successful {
        t.set_artificial_value(v);
    }
    Ok(())
}

fn forward_control_to_main_processor(
    sender: &SenderToNormalThread<ControlMainTask>,
    compartment: Compartment,
    mapping_id: MappingId,
    control_event: ControlEvent<ControlValue>,
    options: ControlOptions,
) {
    let task = ControlMainTask::Control {
        compartment,
        mapping_id,
        event: control_event,
        options,
    };
    // If plug-in dropped, the receiver might be gone already because main processor is
    // unregistered synchronously.
    sender.send_if_space(task);
}

#[allow(clippy::too_many_arguments)]
fn control_main_mappings_virtual(
    main_task_sender: &SenderToNormalThread<ControlMainTask>,
    rt_feedback_sender: &SenderToRealTimeThread<FeedbackRealTimeTask>,
    main_mappings: &mut OrderedMappingMap<RealTimeMapping>,
    value_event: ControlEvent<MidiEvent<VirtualSourceValue>>,
    options: ControlOptions,
    caller: Caller,
    midi_feedback_output: Option<MidiDestination>,
    output_logging_enabled: bool,
    matrix: Option<&WeakMatrix>,
) -> MatchOutcome {
    // Controller mappings can't have virtual sources, so for now we only need to check
    // main mappings.
    let mut match_outcome = MatchOutcome::Unmatched;
    for m in main_mappings
        .values_mut()
        .filter(|m| m.control_is_effectively_on())
    {
        if let CompoundMappingSource::Virtual(s) = &m.source() {
            let midi_event = value_event.payload();
            if let Some(control_value) = s.control(&midi_event.payload()) {
                let _ = process_real_mapping(
                    m,
                    main_task_sender,
                    rt_feedback_sender,
                    Compartment::Main,
                    value_event.with_payload(MidiEvent::new(midi_event.offset(), control_value)),
                    ControlOptions {
                        enforce_target_refresh: match_outcome.matched(),
                        ..options
                    },
                    caller,
                    midi_feedback_output,
                    output_logging_enabled,
                    matrix,
                );
                // If we find an associated main mapping, this is not just consumed, it's matched.
                match_outcome = MatchOutcome::Matched;
            }
        }
    }
    match_outcome
}

fn send_raw_midi_to_fx_output(bytes: &[u8], offset: SampleOffset, caller: Caller) {
    let host = match caller {
        Caller::Vst(h) => h,
        _ => return,
    };
    let event = build_sysex_midi_vst_event(bytes, offset);
    let events = build_vst_events(&event as *const _ as _);
    host.process_events(&events);
}

fn ordered_map_with_capacity<T>(cap: usize) -> OrderedMappingMap<T> {
    let mut map = OrderedMappingMap::with_capacity(cap);
    // This is a workaround for an indexmap bug which allocates space for entries on the
    // first extend/reserve call although it should have been done already when creating
    // it via with_capacity. Remember: We must not allocate in real-time thread!
    map.reserve(0);
    map
}

#[derive(Copy, Clone)]
pub enum IncomingMidiMessage<'a> {
    Short(RawShortMessage),
    SysEx(&'a [u8]),
}

impl<'a> MidiEvent<IncomingMidiMessage<'a>> {
    pub fn from_vst(e: vst::event::Event<'a>) -> Result<Self, &'static str> {
        let msg = IncomingMidiMessage::from_vst(e)?;
        let delta_frames = match e {
            vst::event::Event::Midi(e) => e.delta_frames,
            vst::event::Event::SysEx(e) => e.delta_frames,
            vst::event::Event::Deprecated(e) => e.delta_frames,
        };
        // Negative offset was observed in the wild, see
        // https://github.com/helgoboss/realearn/issues/54. Don't know what that's
        // supposed to mean but falling back to zero should be okay in our case.
        let offset = SampleOffset::new(std::cmp::max(0, delta_frames) as u64);
        Ok(MidiEvent::new(offset, msg))
    }

    pub fn from_reaper(
        e: &'a reaper_medium::MidiEvent,
        sample_rate: Hz,
    ) -> Result<Self, &'static str> {
        let msg = IncomingMidiMessage::from_reaper(e.message())?;
        // Frame offset is given in 1/1024000 of a second, *not* sample frames!
        let offset = SampleOffset::from_frame_offset(e.frame_offset(), sample_rate);
        Ok(MidiEvent::new(offset, msg))
    }
}

impl<'a> IncomingMidiMessage<'a> {
    pub fn from_vst(e: vst::event::Event<'a>) -> Result<Self, &'static str> {
        let res = match e {
            vst::event::Event::Midi(e) => {
                let short_msg = RawShortMessage::from_bytes((
                    e.data[0],
                    e.data[1]
                        .try_into()
                        .map_err(|_| "first data byte invalid")?,
                    e.data[2]
                        .try_into()
                        .map_err(|_| "second data byte invalid")?,
                ));
                let short_msg = short_msg.map_err(|_| "invalid status byte")?;
                IncomingMidiMessage::Short(short_msg)
            }
            vst::event::Event::SysEx(e) => IncomingMidiMessage::SysEx(e.payload),
            vst::event::Event::Deprecated(_) => return Err("deprecated message"),
        };
        Ok(res)
    }

    pub fn from_reaper(m: &'a reaper_medium::MidiMessage) -> Result<Self, &'static str> {
        let res = if m.r#type() == ShortMessageType::SystemExclusiveStart {
            Self::SysEx(m.as_slice())
        } else {
            Self::Short(m.to_other())
        };
        Ok(res)
    }

    fn to_owned(self) -> OwnedIncomingMidiMessage {
        use IncomingMidiMessage::*;
        match self {
            Short(m) => OwnedIncomingMidiMessage::Short(m),
            SysEx(m) => OwnedIncomingMidiMessage::SysEx(m.to_owned()),
        }
    }

    fn might_be_automatically_generated_by_reaper(&self) -> bool {
        match self {
            // TODO-medium Maybe also filter all-sound-off (v6.36+dev0920).
            IncomingMidiMessage::Short(m) => m.r#type() == ShortMessageType::NoteOff,
            IncomingMidiMessage::SysEx(_) => false,
        }
    }

    fn to_source_value(self) -> MidiSourceValue<'a, RawShortMessage> {
        use IncomingMidiMessage::*;
        match self {
            Short(msg) => MidiSourceValue::Plain(msg),
            SysEx(msg) => MidiSourceValue::BorrowedSysEx(msg),
        }
    }
}

#[derive(Copy, Clone, PartialEq, Debug)]
pub struct AudioBlockProps {
    pub block_length: usize,
    pub frame_rate: Hz,
}

impl AudioBlockProps {
    pub fn from_vst(buffer: &vst::buffer::AudioBuffer<f64>, sample_rate: Hz) -> Self {
        Self {
            block_length: buffer.samples(),
            frame_rate: sample_rate,
        }
    }

    pub fn from_on_audio_buffer_args(args: &OnAudioBufferArgs) -> Self {
        Self {
            block_length: args.len as _,
            frame_rate: args.srate,
        }
    }

    pub fn to_playtime(self) -> playtime_clip_engine::rt::BasicAudioRequestProps {
        playtime_clip_engine::rt::BasicAudioRequestProps {
            block_length: self.block_length,
            frame_rate: self.frame_rate,
        }
    }
}

/// Returns whether task still relevant.
fn process_clip_record_task(
    record_task: &mut FxInputClipRecordTask,
    inputs: vst::buffer::Inputs<f64>,
    block_props: BasicAudioRequestProps,
) -> bool {
    let column_source = match record_task.destination.column_source.upgrade() {
        None => return false,
        Some(s) => s,
    };
    let mut src = column_source.lock();
    if !src.recording_poll(record_task.destination.slot_index, block_props) {
        return false;
    }
    let channel_offset = record_task.input.channel_offset().unwrap();
    let write_audio_request =
        RealTimeProcessorWriteAudioRequest::new(inputs, block_props, channel_offset as _);
    src.write_clip_audio(record_task.destination.slot_index, write_audio_request)
        .unwrap();
    true
}

#[derive(Copy, Clone)]
struct RealTimeProcessorWriteAudioRequest<'a> {
    channel_offset: usize,
    inputs: vst::buffer::Inputs<'a, f64>,
    block_props: BasicAudioRequestProps,
}

impl<'a> RealTimeProcessorWriteAudioRequest<'a> {
    pub fn new(
        inputs: vst::buffer::Inputs<'a, f64>,
        block_props: BasicAudioRequestProps,
        channel_offset: usize,
    ) -> Self {
        Self {
            channel_offset,
            inputs,
            block_props,
        }
    }
}

impl<'a> WriteAudioRequest for RealTimeProcessorWriteAudioRequest<'a> {
    fn audio_request_props(&self) -> BasicAudioRequestProps {
        self.block_props
    }

    fn get_channel_buffer(&self, channel_index: usize) -> Option<AudioBuf> {
        let effective_channel_index = self.channel_offset + channel_index;
        if effective_channel_index >= self.inputs.len() {
            return None;
        }
        let slice = self.inputs.get(effective_channel_index);
        AudioBuf::from_slice(slice, 1, self.block_props.block_length).ok()
    }
}

fn log_virtual_control_input(
    sender: &SenderToNormalThread<ControlMainTask>,
    value: ControlEvent<VirtualSourceValue>,
    match_outcome: MatchOutcome,
) {
    sender.send_complaining(ControlMainTask::LogVirtualControlInput {
        event: value,
        match_outcome,
    });
}

fn flatten_control_midi_event<T: Copy>(evt: ControlEvent<MidiEvent<T>>) -> ControlEvent<T> {
    // TODO-medium We could have sample-accurate control event times by converting the MIDI event
    //  sample offset to something like microseconds (according to the current sample rate or by
    //  using REAPER's MidiFrameOffset type instead of SampleOffset in the first place) and using
    //  this microsecond unit in ControlEvent time.
    evt.map_payload(|midi_evt| midi_evt.payload())
}
