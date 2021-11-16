use crate::domain::{
    format_value_as_pan, pan_unit_value, parse_value_from_pan, CompoundChangeEvent, ControlContext,
    HitInstructionReturnValue, MappingControlContext, PanExt, RealearnTarget, ReaperTargetType,
    TargetCharacter, TargetTypeDef, DEFAULT_TARGET,
};
use helgoboss_learn::{
    AbsoluteValue, ControlType, ControlValue, NumericValue, PropValue, Target, UnitValue,
    BASE_EPSILON,
};
use reaper_high::{AvailablePanValue, ChangeEvent, Pan, Project, Track};

#[derive(Clone, Debug, PartialEq)]
pub struct TrackPanTarget {
    pub track: Track,
}

impl RealearnTarget for TrackPanTarget {
    fn control_type_and_character(&self, _: ControlContext) -> (ControlType, TargetCharacter) {
        (ControlType::AbsoluteContinuous, TargetCharacter::Continuous)
    }

    fn parse_as_value(&self, text: &str, _: ControlContext) -> Result<UnitValue, &'static str> {
        parse_value_from_pan(text)
    }

    fn format_value_without_unit(&self, value: UnitValue, _: ControlContext) -> String {
        format_value_as_pan(value)
    }

    fn is_available(&self, _: ControlContext) -> bool {
        self.track.is_available()
    }

    fn hide_formatted_value(&self, _: ControlContext) -> bool {
        true
    }

    fn hide_formatted_step_size(&self, _: ControlContext) -> bool {
        true
    }

    fn project(&self) -> Option<Project> {
        Some(self.track.project())
    }

    fn track(&self) -> Option<&Track> {
        Some(&self.track)
    }

    fn value_unit(&self, _: ControlContext) -> &'static str {
        ""
    }

    fn step_size_unit(&self, _: ControlContext) -> &'static str {
        ""
    }

    fn format_value(&self, value: UnitValue, _: ControlContext) -> String {
        format_value_as_pan(value)
    }

    fn hit(
        &mut self,
        value: ControlValue,
        _: MappingControlContext,
    ) -> Result<HitInstructionReturnValue, &'static str> {
        let pan = Pan::from_normalized_value(value.to_unit_value()?.get());
        self.track.set_pan(pan);
        Ok(None)
    }

    fn process_change_event(
        &self,
        evt: CompoundChangeEvent,
        _: ControlContext,
    ) -> (bool, Option<AbsoluteValue>) {
        match evt {
            CompoundChangeEvent::Reaper(ChangeEvent::TrackPanChanged(e))
                if e.track == self.track =>
            {
                (true, {
                    let pan = match e.new_value {
                        AvailablePanValue::Complete(v) => v.main_pan(),
                        AvailablePanValue::Incomplete(pan) => pan,
                    };
                    Some(AbsoluteValue::Continuous(pan_unit_value(
                        Pan::from_reaper_value(pan),
                    )))
                })
            }
            _ => (false, None),
        }
    }

    fn text_value(&self, _: ControlContext) -> Option<String> {
        Some(self.pan().to_string())
    }

    fn numeric_value(&self, _: ControlContext) -> Option<NumericValue> {
        Some(NumericValue::Decimal(self.pan().reaper_value().get()))
    }

    fn reaper_target_type(&self) -> Option<ReaperTargetType> {
        Some(ReaperTargetType::TrackPan)
    }

    fn prop_value(&self, key: &str, _: ControlContext) -> Option<PropValue> {
        match key {
            "pan.mcu" => {
                let pan = self.pan().reaper_value().get();
                let text = if pan.abs() < BASE_EPSILON {
                    "  <C>  ".to_string()
                } else if pan < 0.0 {
                    format!("<{:>3.0}   ", pan.abs() * 100.0)
                } else {
                    format!("   {:<3.0}>", pan * 100.0)
                };
                Some(PropValue::Text(text))
            }
            _ => None,
        }
    }
}

impl TrackPanTarget {
    fn pan(&self) -> Pan {
        self.track.pan()
    }
}

impl<'a> Target<'a> for TrackPanTarget {
    type Context = ControlContext<'a>;

    fn current_value(&self, _: Self::Context) -> Option<AbsoluteValue> {
        let val = pan_unit_value(self.pan());
        Some(AbsoluteValue::Continuous(val))
    }

    fn control_type(&self, context: Self::Context) -> ControlType {
        self.control_type_and_character(context).0
    }
}

pub const TRACK_PAN_TARGET: TargetTypeDef = TargetTypeDef {
    short_name: "Track pan",
    supports_track: true,
    ..DEFAULT_TARGET
};
