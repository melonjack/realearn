use crate::domain::{InstanceId, MatchOutcome, OwnedIncomingMidiMessage};
use derive_more::Display;
use helgoboss_learn::{
    format_percentage_without_unit, parse_percentage_without_unit, MidiSourceValue, UnitValue,
};
use helgoboss_midi::{RawShortMessage, ShortMessage};
use reaper_high::{Reaper, Volume};
use reaper_medium::Db;
use rosc::{OscMessage, OscPacket};
use std::convert::TryInto;
use std::fmt::{Display, Formatter};

pub fn format_as_percentage_without_unit(value: UnitValue) -> String {
    format_percentage_without_unit(value.get())
}

pub fn format_as_symmetric_percentage_without_unit(value: UnitValue) -> String {
    let symmetric_unit_value = value.get() * 2.0 - 1.0;
    format_percentage_without_unit(symmetric_unit_value)
}

pub fn format_as_double_percentage_without_unit(value: UnitValue) -> String {
    let double_unit_value = value.get() * 2.0;
    format_percentage_without_unit(double_unit_value)
}

pub fn parse_unit_value_from_percentage(text: &str) -> Result<UnitValue, &'static str> {
    parse_percentage_without_unit(text)?.try_into()
}

pub fn parse_from_symmetric_percentage(text: &str) -> Result<UnitValue, &'static str> {
    let percentage: f64 = text.parse().map_err(|_| "not a valid decimal value")?;
    let symmetric_unit_value = percentage / 100.0;
    ((symmetric_unit_value + 1.0) / 2.0).try_into()
}

pub fn parse_from_double_percentage(text: &str) -> Result<UnitValue, &'static str> {
    let percentage: f64 = text.parse().map_err(|_| "not a valid decimal value")?;
    let doble_unit_value = percentage / 100.0;
    (doble_unit_value / 2.0).try_into()
}

pub fn parse_value_from_db(text: &str) -> Result<UnitValue, &'static str> {
    let decimal: f64 = text.parse().map_err(|_| "not a decimal value")?;
    let db: Db = decimal.try_into().map_err(|_| "not in dB range")?;
    Volume::from_db(db).soft_normalized_value().try_into()
}

pub fn format_value_as_db_without_unit(value: UnitValue) -> String {
    let volume = Volume::try_from_soft_normalized_value(value.get()).unwrap_or(Volume::MIN);
    format_volume_as_db_without_unit(volume)
}

pub fn format_volume_as_db_without_unit(volume: Volume) -> String {
    let db = volume.db();
    if db == Db::MINUS_INF {
        "-inf".to_string()
    } else {
        format!("{:.4}", db.get())
    }
}

pub fn db_unit_value(volume: Db) -> UnitValue {
    volume_unit_value(Volume::from_db(volume))
}

pub fn volume_unit_value(volume: Volume) -> UnitValue {
    // The soft-normalized value can be > 1.0, e.g. when we have a volume of 12 dB and then
    // lower the volume fader limit to a lower value. In that case we just report the
    // highest possible value ... not much else we can do.
    UnitValue::new_clamped(volume.soft_normalized_value())
}

pub fn convert_bool_to_unit_value(on: bool) -> UnitValue {
    if on {
        UnitValue::MAX
    } else {
        UnitValue::MIN
    }
}

pub fn format_value_as_db(value: UnitValue) -> String {
    Volume::try_from_soft_normalized_value(value.get())
        .unwrap_or(Volume::MIN)
        .to_string()
}

pub fn format_control_input_with_match_result(
    msg: impl Display,
    match_result: MatchOutcome,
) -> String {
    format!("{} ({})", msg, match_result)
}

pub fn log_virtual_control_input(instance_id: &InstanceId, msg: impl Display) {
    log(instance_id, "Virtual control input", msg);
}

pub fn log_real_control_input(instance_id: &InstanceId, msg: impl Display) {
    log(instance_id, "Real control input", msg);
}

pub fn log_real_learn_input(instance_id: &InstanceId, msg: impl Display) {
    log(instance_id, "Real learn input", msg);
}

pub fn log_virtual_feedback_output(instance_id: &InstanceId, msg: impl Display) {
    log_output(instance_id, OutputReason::VirtualFeedback, msg);
}

pub fn log_real_feedback_output(instance_id: &InstanceId, msg: impl Display) {
    log_output(instance_id, OutputReason::RealFeedback, msg);
}

pub fn log_lifecycle_output(instance_id: &InstanceId, msg: impl Display) {
    log_output(instance_id, OutputReason::Lifecycle, msg);
}

pub fn log_target_output(instance_id: &InstanceId, msg: impl Display) {
    log_output(instance_id, OutputReason::Target, msg);
}

pub fn log_output(instance_id: &InstanceId, reason: OutputReason, msg: impl Display) {
    log(instance_id, reason, msg);
}

#[derive(Copy, Clone, Debug, Display)]
pub enum OutputReason {
    #[display(fmt = "Real feedback output")]
    RealFeedback,
    #[display(fmt = "Virtual feedback output")]
    VirtualFeedback,
    #[display(fmt = "Lifecycle output")]
    Lifecycle,
    #[display(fmt = "Target output")]
    Target,
}

/// Used for logging at the moment.
pub fn format_midi_source_value(value: &MidiSourceValue<RawShortMessage>) -> String {
    use MidiSourceValue::*;
    match value {
        Plain(m) => format_short_midi_message(*m),
        ParameterNumber(m) => serde_json::to_string(&m).unwrap(),
        ControlChange14Bit(m) => serde_json::to_string(&m).unwrap(),
        Tempo(bpm) => format!("{:?}", bpm),
        Raw { events, .. } => {
            let event_strings: Vec<_> = events
                .iter()
                .map(|event| format_raw_midi(event.bytes()))
                .collect();
            event_strings.join(", ")
        }
        BorrowedSysEx(bytes) => format_raw_midi(bytes),
    }
}

pub struct DisplayRawMidi<'a>(pub &'a [u8]);

impl<'a> Display for DisplayRawMidi<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        for (i, b) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{:02X?}", *b)?;
        }
        Ok(())
    }
}

pub fn format_raw_midi(bytes: &[u8]) -> String {
    DisplayRawMidi(bytes).to_string()
}

pub fn format_osc_packet(packet: &OscPacket) -> String {
    format!("{:?}", packet)
}

pub fn format_osc_message(msg: &OscMessage) -> String {
    format!("{:?}", msg)
}

fn format_short_midi_message(msg: RawShortMessage) -> String {
    let bytes = msg.to_bytes();
    let decimal = format!("[{}, {}, {}]", bytes.0, bytes.1, bytes.2);
    let structured = format!("{:?}", msg.to_structured());
    let hex = format!(
        "[{:02X}, {:02X}, {:02X}]",
        bytes.0,
        bytes.1.get(),
        bytes.2.get()
    );
    format!("{} = {} = {}", hex, decimal, structured)
}

pub fn format_incoming_midi_message(msg: OwnedIncomingMidiMessage) -> String {
    use OwnedIncomingMidiMessage::*;
    match msg {
        Short(m) => format_short_midi_message(m),
        SysEx(m) => format_raw_midi(&m),
    }
}

fn log(instance_id: &InstanceId, label: impl Display, msg: impl Display) {
    let reaper = Reaper::get();
    reaper.show_console_msg(format!(
        "{:.3} | ReaLearn {} | {:<16} | {}\n",
        reaper.medium_reaper().low().time_precise(),
        instance_id,
        label,
        msg
    ));
}
