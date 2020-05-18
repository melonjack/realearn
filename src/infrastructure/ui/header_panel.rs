use crate::domain::{MidiControlInput, Property, Session};
use crate::infrastructure::common::bindings::root;
use crate::infrastructure::ui::SessionContext;
use c_str_macro::c_str;
use helgoboss_midi::Channel;
use reaper_high::Reaper;
use reaper_low::Swell;
use reaper_medium::ReaperFunctions;
use rxrust::prelude::*;
use std::cell::{Cell, Ref, RefCell};
use std::rc::{Rc, Weak};
use swell_ui::{View, ViewContext, Window};

/// The upper part of the main panel, containing buttons such as "Add mapping".
#[derive(Debug)]
pub struct HeaderPanel {
    view: ViewContext,
    session: SessionContext,
}

impl HeaderPanel {
    pub fn new(session: SessionContext) -> HeaderPanel {
        HeaderPanel {
            view: Default::default(),
            session,
        }
    }
}

impl HeaderPanel {
    fn learn_source_filter(&self) {
        // TODO
    }

    fn learn_target_filter(&self) {
        // TODO
    }

    fn clear_source_filter(&self) {
        // TODO
    }

    fn clear_target_filter(&self) {
        // TODO
    }

    fn update_let_matched_events_through(&self) {
        self.session.get_mut().let_matched_events_through.set(
            self.view
                .require_control(root::ID_LET_MATCHED_EVENTS_THROUGH_CHECK_BOX)
                .is_checked(),
        );
    }

    fn update_let_unmatched_events_through(&self) {
        self.session.get_mut().let_unmatched_events_through.set(
            self.view
                .require_control(root::ID_LET_UNMATCHED_EVENTS_THROUGH_CHECK_BOX)
                .is_checked(),
        );
    }

    fn update_send_feedback_only_if_armed(&self) {
        self.session.get_mut().send_feedback_only_if_armed.set(
            self.view
                .require_control(root::ID_SEND_FEEDBACK_ONLY_IF_ARMED_CHECK_BOX)
                .is_checked(),
        );
    }

    fn update_always_auto_detect(&self) {
        self.session.get_mut().always_auto_detect.set(
            self.view
                .require_control(root::ID_ALWAYS_AUTO_DETECT_MODE_CHECK_BOX)
                .is_checked(),
        );
    }

    fn update_midi_control_input(&self) {
        // TODO
    }

    fn update_midi_feedback_output(&self) {
        // TODO
    }

    fn invalidate_all_controls(&self) {
        self.invalidate_midi_control_input_combo_box();
        self.invalidate_midi_feedback_output_combo_box();
        self.invalidate_let_matched_events_through_check_box();
        self.invalidate_let_unmatched_events_through_check_box();
        self.invalidate_send_feedback_only_if_armed_check_box();
        self.invalidate_always_auto_detect_check_box();
        self.invalidate_source_filter_buttons();
        self.invalidate_target_filter_buttons();
    }

    fn invalidate_midi_control_input_combo_box(&self) {
        todo!()
    }

    fn invalidate_midi_feedback_output_combo_box(&self) {
        todo!()
    }

    fn invalidate_let_matched_events_through_check_box(&self) {
        let check_box = self
            .view
            .require_control(root::ID_LET_MATCHED_EVENTS_THROUGH_CHECK_BOX);
        if self.session.get().midi_control_input.get() == MidiControlInput::FxInput {
            check_box.enable();
            check_box.set_checked(self.session.get().let_matched_events_through.get());
        } else {
            check_box.disable();
            check_box.uncheck();
        }
    }

    fn invalidate_let_unmatched_events_through_check_box(&self) {
        let check_box = self
            .view
            .require_control(root::ID_LET_UNMATCHED_EVENTS_THROUGH_CHECK_BOX);
        if self.session.get().midi_control_input.get() == MidiControlInput::FxInput {
            check_box.enable();
            check_box.set_checked(self.session.get().let_unmatched_events_through.get());
        } else {
            check_box.disable();
            check_box.uncheck();
        }
    }

    fn invalidate_send_feedback_only_if_armed_check_box(&self) {
        let check_box = self
            .view
            .require_control(root::ID_SEND_FEEDBACK_ONLY_IF_ARMED_CHECK_BOX);
        if self.session.get().is_in_input_fx_chain() {
            check_box.disable();
            check_box.check();
        } else {
            check_box.enable();
            check_box.set_checked(self.session.get().send_feedback_only_if_armed.get());
        }
    }

    fn invalidate_always_auto_detect_check_box(&self) {
        self.view
            .require_control(root::ID_ALWAYS_AUTO_DETECT_MODE_CHECK_BOX)
            .set_checked(self.session.get().always_auto_detect.get());
    }

    fn invalidate_source_filter_buttons(&self) {
        // TODO
    }

    fn invalidate_target_filter_buttons(&self) {
        // TODO
    }

    fn register_listeners(self: Rc<Self>) {
        let session = self.session.get();
        self.view.when(
            &self,
            session.let_matched_events_through.changed(),
            |view| view.invalidate_let_matched_events_through_check_box(),
        );
        self.view.when(
            &self,
            session.let_unmatched_events_through.changed(),
            |view| view.invalidate_let_unmatched_events_through_check_box(),
        );
        self.view.when(
            &self,
            session.send_feedback_only_if_armed.changed(),
            |view| view.invalidate_send_feedback_only_if_armed_check_box(),
        );
        self.view
            .when(&self, session.always_auto_detect.changed(), |view| {
                view.invalidate_always_auto_detect_check_box()
            });
        self.view
            .when(&self, session.midi_control_input.changed(), |view| {
                view.invalidate_midi_control_input_combo_box();
                view.invalidate_let_matched_events_through_check_box();
                view.invalidate_let_unmatched_events_through_check_box();
                let mut session = view.session.get_mut();
                // TODO Seems like we almost always want a copy of the property content
                //  Maybe we should make get() return a copy by default and add get_ref().
                //  Or add a as_ref() like in Option.
                if session.always_auto_detect.get() {
                    let control_input = session.midi_control_input.get();
                    session
                        .send_feedback_only_if_armed
                        .set(control_input != MidiControlInput::FxInput)
                }
            });
        self.view
            .when(&self, session.midi_feedback_output.changed(), |view| {
                view.invalidate_midi_feedback_output_combo_box()
            });
        // TODO sourceFilterListening, targetFilterListening,
    }
}

impl View for HeaderPanel {
    fn dialog_resource_id(&self) -> u32 {
        root::ID_MAPPINGS_DIALOG
    }

    fn view_context(&self) -> &ViewContext {
        &self.view
    }

    fn opened(self: Rc<Self>, window: Window) -> bool {
        self.invalidate_all_controls();
        self.register_listeners();
        true
    }

    fn button_clicked(self: Rc<Self>, resource_id: u32) {
        use root::*;
        match resource_id {
            ID_ADD_MAPPING_BUTTON => self.session.get_mut().add_default_mapping(),
            ID_FILTER_BY_SOURCE_BUTTON => self.learn_source_filter(),
            ID_FILTER_BY_TARGET_BUTTON => self.learn_target_filter(),
            ID_CLEAR_SOURCE_FILTER_BUTTON => self.clear_source_filter(),
            ID_CLEAR_TARGET_FILTER_BUTTON => self.clear_target_filter(),
            ID_IMPORT_BUTTON => self.session.get_mut().import_from_clipboard(),
            ID_EXPORT_BUTTON => self.session.get().export_to_clipboard(),
            ID_SEND_FEEDBACK_BUTTON => self.session.get().send_feedback(),
            ID_LET_MATCHED_EVENTS_THROUGH_CHECK_BOX => self.update_let_matched_events_through(),
            ID_LET_UNMATCHED_EVENTS_THROUGH_CHECK_BOX => self.update_let_unmatched_events_through(),
            ID_SEND_FEEDBACK_ONLY_IF_ARMED_CHECK_BOX => self.update_send_feedback_only_if_armed(),
            ID_ALWAYS_AUTO_DETECT_MODE_CHECK_BOX => self.update_always_auto_detect(),
            _ => {}
        }
    }

    fn option_selected(self: Rc<Self>, resource_id: u32) {
        use root::*;
        match resource_id {
            ID_CONTROL_DEVICE_COMBO_BOX => self.update_midi_control_input(),
            ID_FEEDBACK_DEVICE_COMBO_BOX => self.update_midi_feedback_output(),
            _ => {}
        }
    }
}
