use crate::application::{
    ActivationType, MappingModel, ModifierConditionModel, ProgramConditionModel,
};
use crate::core::default_util::{bool_true, is_bool_true, is_default};
use crate::domain::{MappingCompartment, MappingId, ProcessorContext};
use crate::infrastructure::data::{ModeModelData, SourceModelData, TargetModelData};
use serde::{Deserialize, Serialize};
use std::borrow::BorrowMut;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivationData {
    #[serde(default, skip_serializing_if = "is_default")]
    pub activation_type: ActivationType,
    #[serde(default, skip_serializing_if = "is_default")]
    pub modifier_condition_1: ModifierConditionModel,
    #[serde(default, skip_serializing_if = "is_default")]
    pub modifier_condition_2: ModifierConditionModel,
    #[serde(default, skip_serializing_if = "is_default")]
    pub program_condition: ProgramConditionModel,
    #[serde(default, skip_serializing_if = "is_default")]
    pub eel_condition: String,
}
