use crate::application::BookmarkAnchorType;
use crate::domain::realearn_target::RealearnTarget;
use crate::domain::{
    BackboneState, Compartment, CompartmentParamIndex, CompartmentParams, ExtendedProcessorContext,
    FeedbackResolution, ReaperTarget, UnresolvedActionTarget, UnresolvedAllTrackFxEnableTarget,
    UnresolvedAnyOnTarget, UnresolvedAutomationModeOverrideTarget, UnresolvedClipColumnTarget,
    UnresolvedClipManagementTarget, UnresolvedClipMatrixTarget, UnresolvedClipRowTarget,
    UnresolvedClipSeekTarget, UnresolvedClipTransportTarget, UnresolvedClipVolumeTarget,
    UnresolvedEnableInstancesTarget, UnresolvedEnableMappingsTarget, UnresolvedFxEnableTarget,
    UnresolvedFxNavigateTarget, UnresolvedFxOnlineTarget, UnresolvedFxOpenTarget,
    UnresolvedFxParameterTarget, UnresolvedFxParameterTouchStateTarget, UnresolvedFxPresetTarget,
    UnresolvedGoToBookmarkTarget, UnresolvedLastTouchedTarget, UnresolvedLoadFxSnapshotTarget,
    UnresolvedLoadMappingSnapshotTarget, UnresolvedMidiSendTarget,
    UnresolvedNavigateWithinGroupTarget, UnresolvedOscSendTarget, UnresolvedPlayrateTarget,
    UnresolvedRouteAutomationModeTarget, UnresolvedRouteMonoTarget, UnresolvedRouteMuteTarget,
    UnresolvedRoutePanTarget, UnresolvedRoutePhaseTarget, UnresolvedRouteTouchStateTarget,
    UnresolvedRouteVolumeTarget, UnresolvedSeekTarget, UnresolvedSelectedTrackTarget,
    UnresolvedTempoTarget, UnresolvedTrackArmTarget, UnresolvedTrackAutomationModeTarget,
    UnresolvedTrackMonitoringModeTarget, UnresolvedTrackMuteTarget, UnresolvedTrackPanTarget,
    UnresolvedTrackPeakTarget, UnresolvedTrackPhaseTarget, UnresolvedTrackSelectionTarget,
    UnresolvedTrackShowTarget, UnresolvedTrackSoloTarget, UnresolvedTrackToolTarget,
    UnresolvedTrackTouchStateTarget, UnresolvedTrackVolumeTarget, UnresolvedTrackWidthTarget,
    UnresolvedTransportTarget,
};
use derive_more::{Display, Error};
use enum_dispatch::enum_dispatch;
use enum_iterator::IntoEnumIterator;
use fasteval::{Compiler, Evaler, Instruction, Slab};
use num_enum::{IntoPrimitive, TryFromPrimitive};
use playtime_clip_engine::main::ClipSlotCoordinates;
use realearn_api::schema::ClipColumnTrackContext;
use reaper_high::{
    BookmarkType, FindBookmarkResult, Fx, FxChain, FxParameter, Guid, Project, Reaper,
    SendPartnerType, Track, TrackRoute,
};
use reaper_medium::{BookmarkId, MasterTrackBehavior};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;
use std::fmt::Formatter;
use wildmatch::WildMatch;

/// Maximum number of "allow multiple" resolves (e.g. affected <Selected> tracks).
const MAX_MULTIPLE: usize = 1000;

#[enum_dispatch]
#[derive(Debug)]
pub enum UnresolvedReaperTarget {
    Action(UnresolvedActionTarget),
    FxParameter(UnresolvedFxParameterTarget),
    FxParameterTouchState(UnresolvedFxParameterTouchStateTarget),
    TrackVolume(UnresolvedTrackVolumeTarget),
    TrackTool(UnresolvedTrackToolTarget),
    TrackPeak(UnresolvedTrackPeakTarget),
    TrackSendVolume(UnresolvedRouteVolumeTarget),
    TrackPan(UnresolvedTrackPanTarget),
    TrackWidth(UnresolvedTrackWidthTarget),
    TrackArm(UnresolvedTrackArmTarget),
    TrackSelection(UnresolvedTrackSelectionTarget),
    TrackMute(UnresolvedTrackMuteTarget),
    TrackPhase(UnresolvedTrackPhaseTarget),
    TrackShow(UnresolvedTrackShowTarget),
    TrackSolo(UnresolvedTrackSoloTarget),
    TrackAutomationMode(UnresolvedTrackAutomationModeTarget),
    TrackMonitoringMode(UnresolvedTrackMonitoringModeTarget),
    RoutePan(UnresolvedRoutePanTarget),
    RouteMute(UnresolvedRouteMuteTarget),
    RoutePhase(UnresolvedRoutePhaseTarget),
    RouteMono(UnresolvedRouteMonoTarget),
    RouteAutomationMode(UnresolvedRouteAutomationModeTarget),
    RouteTouchState(UnresolvedRouteTouchStateTarget),
    Tempo(UnresolvedTempoTarget),
    Playrate(UnresolvedPlayrateTarget),
    AutomationModeOverride(UnresolvedAutomationModeOverrideTarget),
    FxEnable(UnresolvedFxEnableTarget),
    FxOnline(UnresolvedFxOnlineTarget),
    FxOpen(UnresolvedFxOpenTarget),
    FxPreset(UnresolvedFxPresetTarget),
    SelectedTrack(UnresolvedSelectedTrackTarget),
    FxNavigate(UnresolvedFxNavigateTarget),
    AllTrackFxEnable(UnresolvedAllTrackFxEnableTarget),
    Transport(UnresolvedTransportTarget),
    LoadFxPreset(UnresolvedLoadFxSnapshotTarget),
    TrackTouchState(UnresolvedTrackTouchStateTarget),
    GoToBookmark(UnresolvedGoToBookmarkTarget),
    Seek(UnresolvedSeekTarget),
    SendMidi(UnresolvedMidiSendTarget),
    SendOsc(UnresolvedOscSendTarget),
    ClipTransport(UnresolvedClipTransportTarget),
    ClipColumn(UnresolvedClipColumnTarget),
    ClipRow(UnresolvedClipRowTarget),
    ClipSeek(UnresolvedClipSeekTarget),
    ClipVolume(UnresolvedClipVolumeTarget),
    ClipManagement(UnresolvedClipManagementTarget),
    ClipMatrix(UnresolvedClipMatrixTarget),
    LoadMappingSnapshot(UnresolvedLoadMappingSnapshotTarget),
    EnableMappings(UnresolvedEnableMappingsTarget),
    NavigateWithinGroup(UnresolvedNavigateWithinGroupTarget),
    EnableInstances(UnresolvedEnableInstancesTarget),
    AnyOn(UnresolvedAnyOnTarget),
    LastTouched(UnresolvedLastTouchedTarget),
}

impl UnresolvedReaperTarget {
    pub fn is_always_active(&self) -> bool {
        matches!(self, Self::LastTouched(_))
    }

    /// Returns whether all conditions for this target to be active are met.
    ///
    /// Targets conditions are for example "track selected" or "FX focused".
    pub fn conditions_are_met(&self, target: &ReaperTarget) -> bool {
        let descriptors = self.unpack_descriptors();
        if let Some(desc) = descriptors.track {
            if desc.enable_only_if_track_selected {
                if let Some(track) = target.track() {
                    if !track.is_selected() {
                        return false;
                    }
                }
            }
        }
        if let Some(desc) = descriptors.fx {
            if desc.enable_only_if_fx_has_focus {
                if let Some(fx) = target.fx() {
                    if !fx.window_has_focus() {
                        return false;
                    }
                }
            }
        }
        true
    }

    /// Should return true if the target should be refreshed (re-resolved) on parameter changes.
    /// Usually true for all targets that use `<Dynamic>` selector.
    pub fn can_be_affected_by_parameters(&self) -> bool {
        let descriptors = self.unpack_descriptors();
        if let Some(desc) = descriptors.track {
            if desc.track.can_be_affected_by_parameters() {
                return true;
            }
        }
        if let Some(desc) = descriptors.fx {
            if desc.fx.can_be_affected_by_parameters() {
                return true;
            }
        }
        if let Some(desc) = descriptors.route {
            if desc.route.can_be_affected_by_parameters() {
                return true;
            }
        }
        if let Some(desc) = descriptors.fx_param {
            if desc.fx_parameter.can_be_affected_by_parameters() {
                return true;
            }
        }
        if let Some(slot) = descriptors.clip_slot {
            if slot.can_be_affected_by_parameters() {
                return true;
            }
        }
        if let Some(col) = descriptors.clip_column {
            if col.can_be_affected_by_parameters() {
                return true;
            }
        }
        if let Some(row) = descriptors.clip_row {
            if row.can_be_affected_by_parameters() {
                return true;
            }
        }
        false
    }

    fn unpack_descriptors(&self) -> Descriptors {
        if let Some(d) = self.fx_parameter_descriptor() {
            return Descriptors {
                track: Some(&d.fx_descriptor.track_descriptor),
                fx: Some(&d.fx_descriptor),
                fx_param: Some(d),
                ..Default::default()
            };
        }
        if let Some(d) = self.fx_descriptor() {
            return Descriptors {
                track: Some(&d.track_descriptor),
                fx: Some(d),
                ..Default::default()
            };
        }
        if let Some(d) = self.route_descriptor() {
            return Descriptors {
                track: Some(&d.track_descriptor),
                route: Some(d),
                ..Default::default()
            };
        }
        if let Some(d) = self.track_descriptor() {
            return Descriptors {
                track: Some(d),
                ..Default::default()
            };
        }
        if let Some(d) = self.clip_slot_descriptor() {
            return Descriptors {
                clip_slot: Some(d),
                ..Default::default()
            };
        }
        if let Some(d) = self.clip_column_descriptor() {
            return Descriptors {
                clip_column: Some(d),
                ..Default::default()
            };
        }
        if let Some(d) = self.clip_row_descriptor() {
            return Descriptors {
                clip_row: Some(d),
                ..Default::default()
            };
        }
        Default::default()
    }
}

pub fn get_effective_tracks(
    context: ExtendedProcessorContext,
    virtual_track: &VirtualTrack,
    compartment: Compartment,
) -> Result<Vec<Track>, &'static str> {
    virtual_track
        .resolve(context, compartment)
        .map_err(|_| "track couldn't be resolved")
}

// Returns an error if that send (or track) doesn't exist.
pub fn get_track_routes(
    context: ExtendedProcessorContext,
    descriptor: &TrackRouteDescriptor,
    compartment: Compartment,
) -> Result<Vec<TrackRoute>, &'static str> {
    let tracks = get_effective_tracks(context, &descriptor.track_descriptor.track, compartment)?;
    let routes = tracks
        .into_iter()
        .flat_map(|track| {
            descriptor
                .route
                .resolve(&track, context, compartment)
                .map_err(|_| "route doesn't exist")
        })
        .collect();
    Ok(routes)
}

#[derive(Debug)]
pub struct TrackDescriptor {
    pub track: VirtualTrack,
    pub enable_only_if_track_selected: bool,
}

#[derive(Debug)]
pub struct FxDescriptor {
    pub track_descriptor: TrackDescriptor,
    pub fx: VirtualFx,
    pub enable_only_if_fx_has_focus: bool,
}

#[derive(Debug)]
pub struct FxParameterDescriptor {
    pub fx_descriptor: FxDescriptor,
    pub fx_parameter: VirtualFxParameter,
}

#[derive(Debug)]
pub struct TrackRouteDescriptor {
    pub track_descriptor: TrackDescriptor,
    pub route: VirtualTrackRoute,
}

impl TrackRouteDescriptor {
    pub fn resolve_first(
        &self,
        context: ExtendedProcessorContext,
        compartment: Compartment,
    ) -> Result<TrackRoute, Box<dyn Error>> {
        let tracks = self.track_descriptor.track.resolve(context, compartment)?;
        let track = tracks.first().ok_or("didn't resolve to any track")?;
        let route = self.route.resolve(track, context, compartment)?;
        Ok(route)
    }
}

#[derive(Debug)]
pub struct VirtualTrackRoute {
    pub r#type: TrackRouteType,
    pub selector: TrackRouteSelector,
}

#[derive(Debug)]
pub enum TrackRouteSelector {
    Dynamic(Box<ExpressionEvaluator>),
    ById(Guid),
    ByName(WildMatch),
    ByIndex(u32),
}

impl TrackRouteSelector {
    pub fn resolve(
        &self,
        track: &Track,
        route_type: TrackRouteType,
        context: ExtendedProcessorContext,
        compartment: Compartment,
    ) -> Result<TrackRoute, TrackRouteResolveError> {
        use TrackRouteSelector::*;
        let route = match self {
            Dynamic(evaluator) => {
                let i = Self::evaluate_to_route_index(evaluator, context, compartment)?;
                resolve_track_route_by_index(track, route_type, i)?
            }
            ById(guid) => {
                let related_track = track.project().track_by_guid(guid);
                let route = find_route_by_related_track(track, &related_track, route_type)?;
                route.ok_or(TrackRouteResolveError::TrackRouteNotFound {
                    guid: Some(*guid),
                    name: None,
                    index: None,
                })?
            }
            ByName(name) => find_route_by_name(track, name, route_type).ok_or_else(|| {
                TrackRouteResolveError::TrackRouteNotFound {
                    guid: None,
                    name: Some(name.clone()),
                    index: None,
                }
            })?,
            ByIndex(i) => resolve_track_route_by_index(track, route_type, *i)?,
        };
        Ok(route)
    }

    pub fn calculated_route_index(
        &self,
        context: ExtendedProcessorContext,
        compartment: Compartment,
    ) -> Option<u32> {
        if let TrackRouteSelector::Dynamic(evaluator) = self {
            Some(Self::evaluate_to_route_index(evaluator, context, compartment).ok()?)
        } else {
            None
        }
    }

    fn evaluate_to_route_index(
        evaluator: &ExpressionEvaluator,
        context: ExtendedProcessorContext,
        compartment: Compartment,
    ) -> Result<u32, TrackRouteResolveError> {
        let compartment_params = context.params().compartment_params(compartment);
        let result = evaluator
            .evaluate(compartment_params)
            .map_err(|_| TrackRouteResolveError::ExpressionFailed)?
            .round() as i32;
        if result < 0 {
            return Err(TrackRouteResolveError::OutOfRange);
        }
        Ok(result as u32)
    }

    pub fn id(&self) -> Option<Guid> {
        use TrackRouteSelector::*;
        match self {
            ById(id) => Some(*id),
            _ => None,
        }
    }

    pub fn index(&self) -> Option<u32> {
        use TrackRouteSelector::*;
        match self {
            ByIndex(i) => Some(*i),
            _ => None,
        }
    }

    pub fn name(&self) -> Option<String> {
        use TrackRouteSelector::*;
        match self {
            ByName(name) => Some(name.to_string()),
            _ => None,
        }
    }
}

impl fmt::Display for VirtualTrackRoute {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        use TrackRouteSelector::*;
        match &self.selector {
            Dynamic(_) => f.write_str("<Dynamic>"),
            ById(id) => write!(f, "{}", id.to_string_without_braces()),
            ByName(name) => write!(f, "\"{}\"", name),
            ByIndex(i) => write!(f, "#{}", i + 1),
        }
    }
}

impl VirtualTrackRoute {
    pub fn resolve(
        &self,
        track: &Track,
        context: ExtendedProcessorContext,
        compartment: Compartment,
    ) -> Result<TrackRoute, TrackRouteResolveError> {
        self.selector
            .resolve(track, self.r#type, context, compartment)
    }

    pub fn id(&self) -> Option<Guid> {
        self.selector.id()
    }

    pub fn index(&self) -> Option<u32> {
        self.selector.index()
    }

    pub fn name(&self) -> Option<String> {
        self.selector.name()
    }

    pub fn can_be_affected_by_parameters(&self) -> bool {
        matches!(
            self,
            VirtualTrackRoute {
                selector: TrackRouteSelector::Dynamic(_),
                ..
            }
        )
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    IntoEnumIterator,
    TryFromPrimitive,
    IntoPrimitive,
    Display,
)]
#[repr(usize)]
pub enum TrackRouteType {
    #[serde(rename = "send")]
    #[display(fmt = "Send")]
    Send,
    #[serde(rename = "receive")]
    #[display(fmt = "Receive")]
    Receive,
    #[serde(rename = "output")]
    #[display(fmt = "Output")]
    HardwareOutput,
}

impl Default for TrackRouteType {
    fn default() -> Self {
        Self::Send
    }
}

#[derive(Debug)]
pub enum VirtualClipSlot {
    Selected,
    ByIndex {
        column_index: usize,
        row_index: usize,
    },
    Dynamic {
        column_evaluator: Box<ExpressionEvaluator>,
        row_evaluator: Box<ExpressionEvaluator>,
    },
}

impl VirtualClipSlot {
    pub fn resolve(
        &self,
        context: ExtendedProcessorContext,
        compartment: Compartment,
    ) -> Result<ClipSlotCoordinates, &'static str> {
        use VirtualClipSlot::*;
        let coordinates = match self {
            Selected => return Err("the concept of a selected slot is not yet supported"),
            ByIndex {
                column_index,
                row_index,
            } => ClipSlotCoordinates::new(*column_index, *row_index),
            Dynamic {
                column_evaluator,
                row_evaluator,
            } => {
                let compartment_params = context.params().compartment_params(compartment);
                let column_index =
                    to_slot_coordinate(column_evaluator.evaluate(compartment_params))?;
                let row_index = to_slot_coordinate(row_evaluator.evaluate(compartment_params))?;
                ClipSlotCoordinates::new(column_index, row_index)
            }
        };
        // let slot_exists = BackboneState::get()
        //     .with_clip_matrix(context.control_context.instance_state, |matrix| {
        //         matrix.slot_exists(coordinates)
        //     })?;
        // if !slot_exists {
        //     return Err("slot doesn't exist");
        // }
        Ok(coordinates)
    }

    pub fn can_be_affected_by_parameters(&self) -> bool {
        matches!(self, VirtualClipSlot::Dynamic { .. })
    }
}

#[derive(Debug)]
pub enum VirtualClipColumn {
    Selected,
    ByIndex(usize),
    Dynamic(Box<ExpressionEvaluator>),
}

impl Default for VirtualClipColumn {
    fn default() -> Self {
        Self::Selected
    }
}

impl VirtualClipColumn {
    pub fn resolve(
        &self,
        context: ExtendedProcessorContext,
        compartment: Compartment,
    ) -> Result<usize, &'static str> {
        use VirtualClipColumn::*;
        let index = match self {
            Selected => return Err("the concept of a selected column is not yet supported"),
            ByIndex(index) => *index,
            Dynamic(evaluator) => {
                let compartment_params = context.params().compartment_params(compartment);
                to_slot_coordinate(evaluator.evaluate(compartment_params))?
            }
        };
        // let column_exists = BackboneState::get()
        //     .with_clip_matrix(context.control_context.instance_state, |matrix| {
        //         index < matrix.column_count()
        //     })?;
        // if !column_exists {
        //     return Err("column doesn't exist");
        // }
        Ok(index)
    }

    pub fn can_be_affected_by_parameters(&self) -> bool {
        matches!(self, VirtualClipColumn::Dynamic { .. })
    }
}

#[derive(Debug)]
pub enum VirtualClipRow {
    Selected,
    ByIndex(usize),
    Dynamic(Box<ExpressionEvaluator>),
}

impl Default for VirtualClipRow {
    fn default() -> Self {
        Self::Selected
    }
}

impl VirtualClipRow {
    pub fn resolve(
        &self,
        context: ExtendedProcessorContext,
        compartment: Compartment,
    ) -> Result<usize, &'static str> {
        use VirtualClipRow::*;
        let index = match self {
            Selected => return Err("the concept of a selected row is not yet supported"),
            ByIndex(index) => *index,
            Dynamic(evaluator) => {
                let compartment_params = context.params().compartment_params(compartment);
                to_slot_coordinate(evaluator.evaluate(compartment_params))?
            }
        };
        // let row_exists = BackboneState::get()
        //     .with_clip_matrix_mut(context.control_context.instance_state, |matrix| {
        //         index < matrix.row_count()
        //     })?;
        // if !row_exists {
        //     return Err("row doesn't exist");
        // }
        Ok(index)
    }

    pub fn can_be_affected_by_parameters(&self) -> bool {
        matches!(self, VirtualClipRow::Dynamic { .. })
    }
}

/// In clip slot targets, the resolve phase makes sure that the targeted slot actually exists.
/// So if we get a `None` value from some of the clip slot methods, it's because the slot doesn't
/// have a clip, which is a valid state and should return *something*. The contract of the target
/// `current_value()` is that if it returns `None`, it means it can't get a value at the moment,
/// probably just temporarily. In that case, the feedback is simply not updated.
pub fn interpret_current_clip_slot_value<T: Default>(value: Option<T>) -> Option<T> {
    Some(value.unwrap_or_default())
}

fn to_slot_coordinate(eval_result: Result<f64, fasteval::Error>) -> Result<usize, &'static str> {
    let res = eval_result.map_err(|_| "couldn't evaluate clip slot coordinate")?;
    if res < 0.0 {
        return Err("negative clip slot coordinate");
    }
    Ok(res.round() as usize)
}

#[derive(Debug)]
pub enum VirtualTrack {
    /// Current track (the one which contains the ReaLearn instance).
    This,
    /// Currently selected track.
    Selected { allow_multiple: bool },
    /// Position in project based on parameter values.
    Dynamic(Box<ExpressionEvaluator>),
    /// Master track.
    Master,
    /// Particular.
    ById(Guid),
    /// Particular.
    ByName {
        wild_match: WildMatch,
        allow_multiple: bool,
    },
    /// Particular.
    ByIndex(u32),
    /// This is the old default for targeting a particular track and it exists solely for backward
    /// compatibility.
    ByIdOrName(Guid, WildMatch),
    /// Uses the track from the given clip column.
    FromClipColumn {
        column: VirtualClipColumn,
        context: ClipColumnTrackContext,
    },
}

#[derive(Debug)]
pub enum VirtualFxParameter {
    Dynamic(Box<ExpressionEvaluator>),
    ByName(WildMatch),
    ById(u32),
    ByIndex(u32),
}

impl VirtualFxParameter {
    pub fn resolve(
        &self,
        fx: &Fx,
        context: ExtendedProcessorContext,
        compartment: Compartment,
    ) -> Result<FxParameter, FxParameterResolveError> {
        use VirtualFxParameter::*;
        match self {
            Dynamic(evaluator) => {
                let i = Self::evaluate_to_fx_parameter_index(evaluator, context, compartment, fx)?;
                resolve_parameter_by_index(fx, i)
            }
            ByName(name) => fx
                .parameters()
                // Parameter names are not reliably UTF-8-encoded (e.g. "JS: Stereo Width")
                .find(|p| name.matches(&p.name().into_inner().to_string_lossy()))
                .ok_or_else(|| FxParameterResolveError::FxParameterNotFound {
                    name: Some(name.clone()),
                    index: None,
                }),
            ByIndex(i) | ById(i) => resolve_parameter_by_index(fx, *i),
        }
    }

    pub fn calculated_fx_parameter_index(
        &self,
        context: ExtendedProcessorContext,
        compartment: Compartment,
        fx: &Fx,
    ) -> Option<u32> {
        if let VirtualFxParameter::Dynamic(evaluator) = self {
            Some(Self::evaluate_to_fx_parameter_index(evaluator, context, compartment, fx).ok()?)
        } else {
            None
        }
    }

    pub fn can_be_affected_by_parameters(&self) -> bool {
        matches!(self, VirtualFxParameter::Dynamic(_))
    }

    fn evaluate_to_fx_parameter_index(
        evaluator: &ExpressionEvaluator,
        context: ExtendedProcessorContext,
        compartment: Compartment,
        fx: &Fx,
    ) -> Result<u32, FxParameterResolveError> {
        let compartment_params = context.params().compartment_params(compartment);
        let result = evaluator
            .evaluate_with_additional_vars(compartment_params, |name, args| match name {
                "tcp_fx_parameter_indexes" => {
                    let i = extract_first_arg_as_positive_integer(args)?;
                    let project = context.context.project_or_current_project();
                    let index = fx
                        .track()
                        .and_then(|t| unsafe {
                            Reaper::get()
                                .medium_reaper()
                                .get_tcp_fx_parm(project.context(), t.raw(), i)
                                .ok()
                        })
                        .map(|res| res.param_index as f64)
                        .unwrap_or(EXPRESSION_NONE_VALUE);
                    Some(index)
                }
                _ => None,
            })
            .map_err(|_| FxParameterResolveError::ExpressionFailed)?
            .round() as i32;
        if result < 0 {
            return Err(FxParameterResolveError::OutOfRange);
        }
        Ok(result as u32)
    }

    pub fn index(&self) -> Option<u32> {
        use VirtualFxParameter::*;
        match self {
            ByIndex(i) | ById(i) => Some(*i),
            _ => None,
        }
    }

    pub fn name(&self) -> Option<String> {
        use VirtualFxParameter::*;
        match self {
            ByName(name) => Some(name.to_string()),
            _ => None,
        }
    }
}

impl fmt::Display for VirtualFxParameter {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        use VirtualFxParameter::*;
        match self {
            Dynamic(_) => f.write_str("<Dynamic>"),
            ByName(name) => write!(f, "\"{}\"", name),
            ByIndex(i) | ById(i) => write!(f, "#{}", i + 1),
        }
    }
}

#[derive(Debug)]
pub struct ExpressionEvaluator {
    slab: Slab,
    instruction: Instruction,
}

impl ExpressionEvaluator {
    pub fn compile(expression: &str) -> Result<ExpressionEvaluator, Box<dyn std::error::Error>> {
        let parser = fasteval::Parser::new();
        let mut slab = fasteval::Slab::new();
        let instruction = parser
            .parse(expression, &mut slab.ps)?
            .from(&slab.ps)
            .compile(&slab.ps, &mut slab.cs);
        let evaluator = Self { slab, instruction };
        Ok(evaluator)
    }

    pub fn evaluate(&self, params: &CompartmentParams) -> Result<f64, fasteval::Error> {
        self.evaluate_internal(params, |_, _| None)
    }

    pub fn evaluate_with_additional_vars(
        &self,
        parameters: &CompartmentParams,
        additional_vars: impl Fn(&str, &[f64]) -> Option<f64>,
    ) -> Result<f64, fasteval::Error> {
        self.evaluate_internal(parameters, additional_vars)
    }

    fn evaluate_internal(
        &self,
        params: &CompartmentParams,
        additional_vars: impl Fn(&str, &[f64]) -> Option<f64>,
    ) -> Result<f64, fasteval::Error> {
        use fasteval::eval_compiled_ref;
        let mut cb = |name: &str, args: Vec<f64>| -> Option<f64> {
            // Use-case specific variables
            if let Some(value) = additional_vars(name, &args) {
                return Some(value);
            }
            match name {
                "none" => Some(EXPRESSION_NONE_VALUE),
                // Parameter array
                "p" => {
                    if let [index] = args.as_slice() {
                        if *index < 0.0 {
                            return None;
                        }
                        let index = index.round() as u32;
                        let index = CompartmentParamIndex::try_from(index).ok()?;
                        Some(params.at(index).effective_value().into())
                    } else {
                        None
                    }
                }
                // Parameter variables (p1, p2, ...)
                _ => {
                    if !name.starts_with('p') {
                        return None;
                    }
                    let one_based_position: u32 = name[1..].parse().ok()?;
                    if one_based_position == 0 {
                        return None;
                    }
                    let index = one_based_position - 1;
                    let index = CompartmentParamIndex::try_from(index).ok()?;
                    Some(params.at(index).effective_value().into())
                }
            }
        };
        let val = eval_compiled_ref!(&self.instruction, &self.slab, &mut cb);
        Ok(val)
    }
}

/// If fasteval encounters usage of a non-existing variable, it fails. Good. But sometimes we need
/// to express that the variable is there and the value is just "None", e.g. in order to use it in a
/// boolean check and react to it accordingly. fasteval doesn't have a
/// dedicated value for that, so we just define an exotic f64 to represent it! We need one that
/// is equal to itself, so NAN and NEG_INFINITY are no options.
pub const EXPRESSION_NONE_VALUE: f64 = f64::MIN;

impl fmt::Display for VirtualTrack {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        use VirtualTrack::*;
        match self {
            This => f.write_str("<This>"),
            Selected { allow_multiple } => f.write_str(if *allow_multiple {
                "<All selected>"
            } else {
                "<Selected>"
            }),
            Master => f.write_str("<Master>"),
            Dynamic(_) => f.write_str("<Dynamic>"),
            ByIdOrName(id, name) => write!(f, "{} or \"{}\"", id.to_string_without_braces(), name),
            ById(id) => write!(f, "{}", id.to_string_without_braces()),
            ByName {
                wild_match,
                allow_multiple,
            } => write!(
                f,
                "\"{}\"{}",
                wild_match,
                if *allow_multiple { " (all)" } else { "" }
            ),
            ByIndex(i) => write!(f, "#{}", i + 1),
            FromClipColumn { .. } => f.write_str("From a clip column"),
        }
    }
}

#[derive(Debug)]
pub enum VirtualFx {
    /// This ReaLearn FX (nice for controlling conditional activation parameters).
    This,
    /// Focused or last focused FX.
    Focused,
    /// Particular FX.
    ChainFx {
        is_input_fx: bool,
        chain_fx: VirtualChainFx,
    },
}

impl fmt::Display for VirtualFx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use VirtualFx::*;
        match self {
            This => f.write_str("<This>"),
            Focused => f.write_str("<Focused>"),
            ChainFx {
                chain_fx,
                is_input_fx,
            } => {
                chain_fx.fmt(f)?;
                if *is_input_fx {
                    f.write_str(" (input FX)")?;
                }
                Ok(())
            }
        }
    }
}

impl VirtualFx {
    pub fn id(&self) -> Option<Guid> {
        match self {
            VirtualFx::This => None,
            VirtualFx::Focused => None,
            VirtualFx::ChainFx { chain_fx, .. } => chain_fx.id(),
        }
    }

    pub fn is_input_fx(&self) -> bool {
        match self {
            // In case of <This>, it doesn't matter.
            VirtualFx::This => false,
            VirtualFx::Focused => false,
            VirtualFx::ChainFx { is_input_fx, .. } => *is_input_fx,
        }
    }

    pub fn index(&self) -> Option<u32> {
        match self {
            VirtualFx::This => None,
            VirtualFx::Focused => None,
            VirtualFx::ChainFx { chain_fx, .. } => chain_fx.index(),
        }
    }

    pub fn name(&self) -> Option<String> {
        match self {
            VirtualFx::This => None,
            VirtualFx::Focused => None,
            VirtualFx::ChainFx { chain_fx, .. } => chain_fx.name(),
        }
    }

    pub fn can_be_affected_by_parameters(&self) -> bool {
        matches!(
            self,
            VirtualFx::ChainFx {
                chain_fx: VirtualChainFx::Dynamic(_),
                ..
            }
        )
    }
}

impl VirtualTrack {
    pub fn resolve(
        &self,
        context: ExtendedProcessorContext,
        compartment: Compartment,
    ) -> Result<Vec<Track>, TrackResolveError> {
        use VirtualTrack::*;
        let project = context.context().project_or_current_project();
        let tracks = match self {
            This => {
                let single = context
                    .context()
                    .containing_fx()
                    .track()
                    .cloned()
                    // If this is monitoring FX, we want this to resolve to the master track since
                    // in most functions, monitoring FX chain is the "input FX chain" of the master
                    // track.
                    .unwrap_or_else(|| project.master_track());
                vec![single]
            }
            Selected { allow_multiple } => project
                .selected_tracks(MasterTrackBehavior::IncludeMasterTrack)
                .take(if *allow_multiple { MAX_MULTIPLE } else { 1 })
                .collect(),
            Dynamic(evaluator) => {
                let index = Self::evaluate_to_track_index(evaluator, context, compartment)?;
                let single = resolve_track_by_index(project, index)?;
                vec![single]
            }
            Master => vec![project.master_track()],
            ByIdOrName(guid, name) => {
                let t = project.track_by_guid(guid);
                let single = if t.is_available() {
                    t
                } else {
                    find_track_by_name(project, name).ok_or(TrackResolveError::TrackNotFound {
                        guid: Some(*guid),
                        name: Some(name.clone()),
                        index: None,
                    })?
                };
                vec![single]
            }
            ById(guid) => {
                let single = project.track_by_guid(guid);
                if !single.is_available() {
                    return Err(TrackResolveError::TrackNotFound {
                        guid: Some(*guid),
                        name: None,
                        index: None,
                    });
                }
                vec![single]
            }
            ByName {
                wild_match,
                allow_multiple,
            } => find_tracks_by_name(project, wild_match)
                .take(if *allow_multiple { MAX_MULTIPLE } else { 1 })
                .collect(),
            ByIndex(index) => {
                let single = resolve_track_by_index(project, *index as i32)?;
                vec![single]
            }
            FromClipColumn {
                column,
                context: track_context,
            } => {
                // TODO-low Not very helpful. Do we even use this error type?
                let generic_error = || TrackResolveError::TrackNotFound {
                    guid: None,
                    name: None,
                    index: None,
                };
                let clip_column_index = column
                    .resolve(context, compartment)
                    .map_err(|_| generic_error())?;
                let track = BackboneState::get()
                    .with_clip_matrix(context.control_context.instance_state, |matrix| {
                        let column = matrix.column(clip_column_index)?;
                        match track_context {
                            ClipColumnTrackContext::Playback => column.playback_track().cloned(),
                            ClipColumnTrackContext::Recording => column.effective_recording_track(),
                        }
                    })
                    .map_err(|_| generic_error())?
                    .map_err(|_| generic_error())?;
                vec![track]
            }
        };
        Ok(tracks)
    }

    #[allow(clippy::match_like_matches_macro)]
    pub fn can_be_affected_by_parameters(&self) -> bool {
        match self {
            VirtualTrack::Dynamic(_) => true,
            VirtualTrack::FromClipColumn {
                column: VirtualClipColumn::Dynamic(_),
                ..
            } => true,
            _ => false,
        }
    }

    pub fn calculated_track_index(
        &self,
        context: ExtendedProcessorContext,
        compartment: Compartment,
    ) -> Option<i32> {
        if let VirtualTrack::Dynamic(evaluator) = self {
            Some(Self::evaluate_to_track_index(evaluator, context, compartment).ok()?)
        } else {
            None
        }
    }

    fn evaluate_to_track_index(
        evaluator: &ExpressionEvaluator,
        context: ExtendedProcessorContext,
        compartment: Compartment,
    ) -> Result<i32, TrackResolveError> {
        let compartment_params = context.params().compartment_params(compartment);
        let result = evaluator
            .evaluate_with_additional_vars(compartment_params, |name, args| match name {
                "this_track_index" => {
                    let track = context.context().track()?;
                    Some(get_track_index_for_expression(track))
                }
                "selected_track_index" => {
                    let index = context
                        .context()
                        .project_or_current_project()
                        .first_selected_track(MasterTrackBehavior::IncludeMasterTrack)
                        .as_ref()
                        .map(get_track_index_for_expression);
                    Some(index.unwrap_or(EXPRESSION_NONE_VALUE))
                }
                "selected_track_indexes" => {
                    let i = extract_first_arg_as_positive_integer(args)?;
                    let reaper = Reaper::get().medium_reaper();
                    let project = context.context().project_or_current_project();
                    let raw_track = reaper.get_selected_track_2(
                        project.context(),
                        i,
                        MasterTrackBehavior::IncludeMasterTrack,
                    );
                    match raw_track {
                        None => Some(EXPRESSION_NONE_VALUE),
                        Some(raw_track) => {
                            let t = Track::new(raw_track, Some(project.raw()));
                            Some(get_track_index_for_expression(&t))
                        }
                    }
                }
                _ => None,
            })
            .map_err(|_| TrackResolveError::ExpressionFailed)?
            .round() as i32;
        if result < -1 {
            return Err(TrackResolveError::OutOfRange);
        }
        Ok(result as i32)
    }

    pub fn id(&self) -> Option<Guid> {
        use VirtualTrack::*;
        match self {
            ById(id) | ByIdOrName(id, _) => Some(*id),
            _ => None,
        }
    }

    pub fn index(&self) -> Option<u32> {
        use VirtualTrack::*;
        match self {
            ByIndex(i) => Some(*i),
            _ => None,
        }
    }

    pub fn name(&self) -> Option<String> {
        use VirtualTrack::*;
        match self {
            ByName {
                wild_match: name, ..
            }
            | ByIdOrName(_, name) => Some(name.to_string()),
            _ => None,
        }
    }

    pub fn clip_column(&self) -> Option<&VirtualClipColumn> {
        if let VirtualTrack::FromClipColumn { column, .. } = self {
            Some(column)
        } else {
            None
        }
    }

    pub fn clip_column_track_context(&self) -> Option<ClipColumnTrackContext> {
        if let VirtualTrack::FromClipColumn { context, .. } = self {
            Some(*context)
        } else {
            None
        }
    }
}

#[derive(Debug)]
pub enum VirtualChainFx {
    /// Position in FX chain based on parameter values.
    Dynamic(Box<ExpressionEvaluator>),
    /// This is the new default.
    ///
    /// The index is just used as performance hint, not as fallback.
    ById(Guid, Option<u32>),
    ByName {
        wild_match: WildMatch,
        allow_multiple: bool,
    },
    ByIndex(u32),
    /// This is the old default.
    ///
    /// The index comes into play as fallback whenever track is "<Selected>" or the GUID can't be
    /// determined (is `None`). I'm not sure how latter is possible but I keep it for backward
    /// compatibility.
    ByIdOrIndex(Option<Guid>, u32),
}

impl fmt::Display for VirtualChainFx {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        use VirtualChainFx::*;
        match self {
            Dynamic(_) => f.write_str("<Dynamic>"),
            ById(guid, _) => {
                write!(f, "{}", guid.to_string_without_braces())
            }
            ByName {
                wild_match,
                allow_multiple,
            } => write!(
                f,
                "\"{}\"{}",
                wild_match,
                if *allow_multiple { " (all)" } else { "" }
            ),
            ByIdOrIndex(None, i) | ByIndex(i) => write!(f, "#{}", i + 1),
            ByIdOrIndex(Some(guid), i) => {
                write!(f, "{} ({})", guid.to_string_without_braces(), i + 1)
            }
        }
    }
}

fn find_track_by_name(project: Project, name: &WildMatch) -> Option<Track> {
    project.tracks().find(|t| match t.name() {
        None => false,
        Some(n) => name.matches(n.to_str()),
    })
}

fn find_tracks_by_name(project: Project, name: &WildMatch) -> impl Iterator<Item = Track> + '_ {
    project.tracks().filter(move |t| match t.name() {
        None => false,
        Some(n) => name.matches(n.to_str()),
    })
}

#[derive(Clone, Debug, Display, Error)]
pub enum TrackResolveError {
    #[display(fmt = "ExpressionFailed")]
    ExpressionFailed,
    #[display(fmt = "OutOfRange")]
    OutOfRange,
    #[display(fmt = "TrackNotFound")]
    TrackNotFound {
        guid: Option<Guid>,
        name: Option<WildMatch>,
        index: Option<u32>,
    },
    NoTrackSelected,
}

#[derive(Clone, Debug, Display, Error)]
pub enum FxParameterResolveError {
    #[display(fmt = "ExpressionFailed")]
    ExpressionFailed,
    #[display(fmt = "OutOfRange")]
    OutOfRange,
    #[display(fmt = "FxParameterNotFound")]
    FxParameterNotFound {
        name: Option<WildMatch>,
        index: Option<u32>,
    },
}

#[derive(Clone, Debug, Display, Error)]
pub enum TrackRouteResolveError {
    #[display(fmt = "ExpressionFailed")]
    ExpressionFailed,
    #[display(fmt = "OutOfRange")]
    OutOfRange,
    #[display(fmt = "InvalidRoute")]
    InvalidRoute,
    #[display(fmt = "TrackRouteNotFound")]
    TrackRouteNotFound {
        guid: Option<Guid>,
        name: Option<WildMatch>,
        index: Option<u32>,
    },
}

impl VirtualChainFx {
    pub fn resolve(
        &self,
        fx_chains: &[FxChain],
        context: ExtendedProcessorContext,
        compartment: Compartment,
    ) -> Result<Vec<Fx>, FxResolveError> {
        use VirtualChainFx::*;
        let fxs = match self {
            Dynamic(evaluator) => fx_chains
                .iter()
                .flat_map(|fx_chain| {
                    let index =
                        Self::evaluate_to_fx_index(evaluator, context, compartment, fx_chain)?;
                    get_index_based_fx_on_chain(fx_chain, index).map_err(|_| {
                        FxResolveError::FxNotFound {
                            guid: None,
                            name: None,
                            index: Some(index),
                        }
                    })
                })
                .collect(),
            ById(guid, index) => {
                let fx_not_found_error = || FxResolveError::FxNotFound {
                    guid: Some(*guid),
                    name: None,
                    index: None,
                };
                // It doesn't make sense to search for the same FX ID on multiple tracks, so we
                // only take the first one.
                let fx_chain = fx_chains.first().ok_or_else(fx_not_found_error)?;
                let single =
                    get_guid_based_fx_by_guid_on_chain_with_index_hint(fx_chain, guid, *index)
                        .map_err(|_| fx_not_found_error())?;
                vec![single]
            }
            ByName {
                wild_match,
                allow_multiple,
            } => find_fxs_by_name(fx_chains, wild_match)
                .take(if *allow_multiple { MAX_MULTIPLE } else { 1 })
                .collect(),
            ByIndex(index) | ByIdOrIndex(None, index) => fx_chains
                .iter()
                .flat_map(|fx_chain| {
                    get_index_based_fx_on_chain(fx_chain, *index).map_err(|_| {
                        FxResolveError::FxNotFound {
                            guid: None,
                            name: None,
                            index: Some(*index),
                        }
                    })
                })
                .collect(),
            ByIdOrIndex(Some(guid), index) => {
                let fx_not_found_error = || FxResolveError::FxNotFound {
                    guid: Some(*guid),
                    name: None,
                    index: Some(*index),
                };
                // It doesn't make sense to search for the same FX ID on multiple tracks, so we
                // only take the first one.
                let fx_chain = fx_chains.first().ok_or_else(fx_not_found_error)?;
                // Track by GUID because target relates to a very particular FX
                let single = get_guid_based_fx_by_guid_on_chain_with_index_hint(
                    fx_chain,
                    guid,
                    Some(*index),
                )
                // Fall back to index-based
                .or_else(|_| get_index_based_fx_on_chain(fx_chain, *index))
                .map_err(|_| fx_not_found_error())?;
                vec![single]
            }
        };
        Ok(fxs)
    }

    pub fn calculated_fx_index(
        &self,
        context: ExtendedProcessorContext,
        compartment: Compartment,
        chain: &FxChain,
    ) -> Option<u32> {
        if let VirtualChainFx::Dynamic(evaluator) = self {
            Some(Self::evaluate_to_fx_index(evaluator, context, compartment, chain).ok()?)
        } else {
            None
        }
    }

    fn evaluate_to_fx_index(
        evaluator: &ExpressionEvaluator,
        context: ExtendedProcessorContext,
        compartment: Compartment,
        chain: &FxChain,
    ) -> Result<u32, FxResolveError> {
        let compartment_params = context.params().compartment_params(compartment);
        let result = evaluator
            .evaluate_with_additional_vars(compartment_params, |name, args| match name {
                "tcp_fx_indexes" => {
                    let i = extract_first_arg_as_positive_integer(args)?;
                    if chain.is_input_fx() {
                        // Only FX parameters from the normal FX chain can be displayed in TCP.
                        return Some(EXPRESSION_NONE_VALUE);
                    }
                    let project = context.context.project_or_current_project();
                    let index = chain
                        .track()
                        .and_then(|t| unsafe {
                            Reaper::get()
                                .medium_reaper()
                                .get_tcp_fx_parm(project.context(), t.raw(), i)
                                .ok()
                        })
                        .map(|res| res.fx_location.to_raw() as f64)
                        .unwrap_or(EXPRESSION_NONE_VALUE);
                    Some(index)
                }
                _ => None,
            })
            .map_err(|_| FxResolveError::ExpressionFailed)?
            .round() as i32;
        if result < 0 {
            return Err(FxResolveError::OutOfRange);
        }
        Ok(result as u32)
    }

    pub fn id(&self) -> Option<Guid> {
        use VirtualChainFx::*;
        match self {
            ById(id, _) => Some(*id),
            ByIdOrIndex(id, _) => *id,
            _ => None,
        }
    }

    pub fn index(&self) -> Option<u32> {
        use VirtualChainFx::*;
        match self {
            ByIndex(i) | ByIdOrIndex(_, i) => Some(*i),
            ById(_, index_hint) => *index_hint,
            _ => None,
        }
    }

    pub fn name(&self) -> Option<String> {
        use VirtualChainFx::*;
        match self {
            ByName { wild_match, .. } => Some(wild_match.to_string()),
            _ => None,
        }
    }
}

fn find_fxs_by_name<'a>(
    chains: &'a [FxChain],
    name: &'a WildMatch,
) -> impl Iterator<Item = Fx> + 'a {
    chains
        .iter()
        .flat_map(|chain| chain.fxs())
        .filter(move |fx| name.matches(fx.name().to_str()))
}

#[derive(Clone, Debug, Display, Error)]
pub enum FxResolveError {
    #[display(fmt = "ExpressionFailed")]
    ExpressionFailed,
    #[display(fmt = "OutOfRange")]
    OutOfRange,
    #[display(fmt = "FxNotFound")]
    FxNotFound {
        guid: Option<Guid>,
        name: Option<WildMatch>,
        index: Option<u32>,
    },
}

pub fn get_non_present_virtual_track_label(track: &VirtualTrack) -> String {
    format!("<Not present> ({})", track)
}

pub fn get_non_present_virtual_route_label(route: &VirtualTrackRoute) -> String {
    format!("<Not present> ({})", route)
}

// Returns an error if that param (or FX) doesn't exist.
pub fn get_fx_params(
    context: ExtendedProcessorContext,
    fx_parameter_descriptor: &FxParameterDescriptor,
    compartment: Compartment,
) -> Result<Vec<FxParameter>, &'static str> {
    let fxs = get_fxs(context, &fx_parameter_descriptor.fx_descriptor, compartment)?;
    let parameters = fxs
        .into_iter()
        .flat_map(|fx| {
            fx_parameter_descriptor
                .fx_parameter
                .resolve(&fx, context, compartment)
                .map_err(|_| "parameter doesn't exist")
        })
        .collect();
    Ok(parameters)
}

// Returns an error if the FX doesn't exist.
pub fn get_fxs(
    context: ExtendedProcessorContext,
    descriptor: &FxDescriptor,
    compartment: Compartment,
) -> Result<Vec<Fx>, &'static str> {
    match &descriptor.fx {
        VirtualFx::This => {
            let fx = context.context().containing_fx();
            if fx.is_available() {
                Ok(vec![fx.clone()])
            } else {
                Err("this FX not available anymore")
            }
        }
        VirtualFx::Focused => {
            let single = Reaper::get()
                .focused_fx()
                .ok_or("couldn't get (last) focused FX")?;
            Ok(vec![single])
        }
        VirtualFx::ChainFx {
            is_input_fx,
            chain_fx,
        } => {
            enum MaybeOwned<'a, T> {
                Owned(T),
                Borrowed(&'a T),
            }
            impl<'a, T> MaybeOwned<'a, T> {
                fn get(&self) -> &T {
                    match self {
                        MaybeOwned::Owned(o) => o,
                        MaybeOwned::Borrowed(b) => b,
                    }
                }
            }
            let chain_fx = match chain_fx {
                VirtualChainFx::ByIdOrIndex(_, index) => {
                    // Actually it's not that important whether we create an index-based or
                    // GUID-based FX. The session listeners will recreate and
                    // resync the FX whenever something has changed anyway. But
                    // for monitoring FX it could still be good (which we don't get notified
                    // about unfortunately).
                    if matches!(
                        descriptor.track_descriptor.track,
                        VirtualTrack::Selected { .. }
                    ) {
                        MaybeOwned::Owned(VirtualChainFx::ByIndex(*index))
                    } else {
                        MaybeOwned::Borrowed(chain_fx)
                    }
                }
                _ => MaybeOwned::Borrowed(chain_fx),
            };
            let fx_chains = get_fx_chains(
                context,
                &descriptor.track_descriptor.track,
                *is_input_fx,
                compartment,
            )?;
            chain_fx
                .get()
                .resolve(&fx_chains, context, compartment)
                .map_err(|_| "couldn't resolve particular FX")
        }
    }
}

fn get_index_based_fx_on_chain(fx_chain: &FxChain, fx_index: u32) -> Result<Fx, &'static str> {
    let fx = fx_chain.fx_by_index_untracked(fx_index);
    if !fx.is_available() {
        return Err("no FX at that index");
    }
    Ok(fx)
}

fn resolve_parameter_by_index(fx: &Fx, index: u32) -> Result<FxParameter, FxParameterResolveError> {
    let param = fx.parameter_by_index(index);
    if !param.is_available() {
        return Err(FxParameterResolveError::FxParameterNotFound {
            name: None,
            index: Some(index),
        });
    }
    Ok(param)
}

fn resolve_track_by_index(project: Project, index: i32) -> Result<Track, TrackResolveError> {
    if index >= 0 {
        let i = index as u32;
        project
            .track_by_index(i)
            .ok_or(TrackResolveError::TrackNotFound {
                guid: None,
                name: None,
                index: Some(i),
            })
    } else {
        Ok(project.master_track())
    }
}

pub fn resolve_track_route_by_index(
    track: &Track,
    route_type: TrackRouteType,
    index: u32,
) -> Result<TrackRoute, TrackRouteResolveError> {
    let option = match route_type {
        TrackRouteType::Send => track.typed_send_by_index(SendPartnerType::Track, index),
        TrackRouteType::Receive => track.receive_by_index(index),
        TrackRouteType::HardwareOutput => {
            track.typed_send_by_index(SendPartnerType::HardwareOutput, index)
        }
    };
    if let Some(route) = option {
        Ok(route)
    } else {
        Err(TrackRouteResolveError::TrackRouteNotFound {
            guid: None,
            name: None,
            index: Some(index),
        })
    }
}

pub fn get_fx_chains(
    context: ExtendedProcessorContext,
    track: &VirtualTrack,
    is_input_fx: bool,
    compartment: Compartment,
) -> Result<Vec<FxChain>, &'static str> {
    let fx_chains = get_effective_tracks(context, track, compartment)?
        .into_iter()
        .map(|track| get_fx_chain(track, is_input_fx))
        .collect();
    Ok(fx_chains)
}

fn get_fx_chain(track: Track, is_input_fx: bool) -> FxChain {
    if is_input_fx {
        if track.is_master_track() {
            // The combination "Master track + input FX chain" by convention represents the
            // monitoring FX chain in REAPER. It's a bit unfortunate that we have 2 representations
            // of the same thing: A special monitoring FX enum variant and this convention.
            // E.g. it leads to the result that both representations are not equal from a reaper-rs
            // perspective. We should enforce the enum variant whenever possible because the
            // convention is somehow flawed. E.g. what if we have 2 master tracks of different
            // projects? This should be done in reaper-high, there's already a to-do there.
            Reaper::get().monitoring_fx_chain()
        } else {
            track.input_fx_chain()
        }
    } else {
        track.normal_fx_chain()
    }
}

fn get_guid_based_fx_by_guid_on_chain_with_index_hint(
    fx_chain: &FxChain,
    guid: &Guid,
    fx_index: Option<u32>,
) -> Result<Fx, &'static str> {
    let fx = if let Some(i) = fx_index {
        fx_chain.fx_by_guid_and_index(guid, i)
    } else {
        fx_chain.fx_by_guid(guid)
    };
    // is_available() also invalidates the index if necessary
    // TODO-low This is too implicit.
    if !fx.is_available() {
        return Err("no FX with that GUID");
    }
    Ok(fx)
}

pub fn find_bookmark(
    project: Project,
    bookmark_type: BookmarkType,
    anchor_type: BookmarkAnchorType,
    bookmark_ref: u32,
) -> Result<FindBookmarkResult, &'static str> {
    if !project.is_available() {
        return Err("project not available");
    }
    match anchor_type {
        BookmarkAnchorType::Index => project
            .find_bookmark_by_type_and_index(bookmark_type, bookmark_ref)
            .ok_or("bookmark with that type and index not found"),
        BookmarkAnchorType::Id => project
            .find_bookmark_by_type_and_id(bookmark_type, BookmarkId::new(bookmark_ref))
            .ok_or("bookmark with that type and ID not found"),
    }
}

fn find_route_by_related_track(
    main_track: &Track,
    related_track: &Track,
    route_type: TrackRouteType,
) -> Result<Option<TrackRoute>, TrackRouteResolveError> {
    let option = match route_type {
        TrackRouteType::Send => main_track.find_send_by_destination_track(related_track),
        TrackRouteType::Receive => main_track.find_receive_by_source_track(related_track),
        TrackRouteType::HardwareOutput => {
            return Err(TrackRouteResolveError::InvalidRoute);
        }
    };
    Ok(option)
}

fn find_route_by_name(
    track: &Track,
    name: &WildMatch,
    route_type: TrackRouteType,
) -> Option<TrackRoute> {
    let matcher = |r: &TrackRoute| name.matches(r.name().to_str());
    match route_type {
        TrackRouteType::Send => track.typed_sends(SendPartnerType::Track).find(matcher),
        TrackRouteType::Receive => track.receives().find(matcher),
        TrackRouteType::HardwareOutput => track
            .typed_sends(SendPartnerType::HardwareOutput)
            .find(matcher),
    }
}

#[derive(Default)]
struct Descriptors<'a> {
    track: Option<&'a TrackDescriptor>,
    fx: Option<&'a FxDescriptor>,
    route: Option<&'a TrackRouteDescriptor>,
    fx_param: Option<&'a FxParameterDescriptor>,
    clip_slot: Option<&'a VirtualClipSlot>,
    clip_column: Option<&'a VirtualClipColumn>,
    clip_row: Option<&'a VirtualClipRow>,
}

#[enum_dispatch(UnresolvedReaperTarget)]
pub trait UnresolvedReaperTargetDef {
    fn is_always_active(&self) -> bool {
        false
    }

    fn resolve(
        &self,
        context: ExtendedProcessorContext,
        compartment: Compartment,
    ) -> Result<Vec<ReaperTarget>, &'static str>;

    /// `None` means that no polling is necessary for feedback because we are notified via events.
    fn feedback_resolution(&self) -> Option<FeedbackResolution> {
        None
    }

    /// Should return true if the target should be refreshed (reresolved) on changes such as track
    /// selection etc. (see [`ReaperTarget::is_potential_change_event`]). If in doubt or too lazy to
    /// make a distinction depending on the selector, better return true! This makes sure things
    /// stay up-to-date. Doing an unnecessary refreshment can have the following effects:
    /// - Slightly reduce performance: Not refreshing is of course cheaper (but resolving is
    ///   generally fast so this shouldn't matter)
    /// - Removes target state: If the resolved target contains state, it's going to be disappear
    ///   when the target is resolved again. Matters for some targets (but usually not).
    fn can_be_affected_by_change_events(&self) -> bool {
        true
    }

    fn track_descriptor(&self) -> Option<&TrackDescriptor> {
        None
    }

    fn fx_descriptor(&self) -> Option<&FxDescriptor> {
        None
    }

    fn route_descriptor(&self) -> Option<&TrackRouteDescriptor> {
        None
    }

    fn fx_parameter_descriptor(&self) -> Option<&FxParameterDescriptor> {
        None
    }

    fn clip_slot_descriptor(&self) -> Option<&VirtualClipSlot> {
        None
    }

    fn clip_column_descriptor(&self) -> Option<&VirtualClipColumn> {
        None
    }

    fn clip_row_descriptor(&self) -> Option<&VirtualClipRow> {
        None
    }
}

/// Special: Index -1 means master track.
fn get_track_index_for_expression(track: &Track) -> f64 {
    track.index().map(|i| i as f64).unwrap_or(-1.0)
}

fn extract_first_arg_as_positive_integer(args: &[f64]) -> Option<u32> {
    let i = match args {
        [i] => i,
        _ => return None,
    };
    if *i < 0.0 {
        return None;
    }
    Some(i.round() as u32)
}
