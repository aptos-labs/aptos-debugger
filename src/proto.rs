use dap::{
    events::{Event, StoppedEventBody},
    types::{StoppedEventReason, Variable},
};
use crate::debugger::StopReason;

pub(crate) fn stop_reason_to_dap(reason: &StopReason) -> StoppedEventReason {
    match reason {
        StopReason::Entry => StoppedEventReason::Entry,
        StopReason::Step => StoppedEventReason::Step,
        StopReason::Breakpoint(_) => StoppedEventReason::Breakpoint,
    }
}

pub(crate) fn stopped_event(reason: StoppedEventReason) -> Event {
    Event::Stopped(StoppedEventBody {
        reason,
        description: None,
        thread_id: Some(1),
        preserve_focus_hint: None,
        text: None,
        all_threads_stopped: Some(true),
        hit_breakpoint_ids: None,
    })
}

pub(crate) fn var(name: impl Into<String>, value: impl Into<String>) -> Variable {
    Variable {
        name: name.into(),
        value: value.into(),
        type_field: None,
        presentation_hint: None,
        evaluate_name: None,
        variables_reference: 0,
        named_variables: None,
        indexed_variables: None,
        memory_reference: None,
    }
}
