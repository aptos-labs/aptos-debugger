use move_vm_debug::DebugValue;

#[derive(Debug)]
pub enum DapCommand {
    Continue,
    Step(usize),
    StepOver(usize),
    StepOut,
    SetBreakpoints(Vec<String>),
}

#[derive(Debug)]
pub enum StopReason {
    Entry,
    Step,
    Breakpoint(String),
}

#[derive(Debug)]
pub struct DapFrameInfo {
    pub function_name: String,
    pub pc: u16,
    pub source_location: Option<String>,
    pub locals: Vec<DapLocalInfo>,
}

#[derive(Debug)]
pub struct DapLocalInfo {
    pub index: usize,
    pub name: String,
    pub type_name: String,
    pub value: DebugValue,
}

#[derive(Debug)]
pub struct VmStoppedState {
    pub function_name: String,
    pub pc: u16,
    pub instruction: String,
    pub dap_stack_trace: Vec<DapFrameInfo>,
    pub dap_locals: Vec<DapLocalInfo>,
    pub source_location: Option<String>,
}

#[derive(Debug)]
pub enum DapEvent {
    Stopped {
        reason: StopReason,
        vm_state: VmStoppedState,
    },
    Terminated {
        message: Option<String>,
    },
}

#[derive(Clone)]
pub struct DapDebugHandle {
    pub event_tx: crossbeam_channel::Sender<DapEvent>,
    pub command_rx: crossbeam_channel::Receiver<DapCommand>,
}

pub fn create_dap_channels() -> (
    crossbeam_channel::Sender<DapCommand>,
    crossbeam_channel::Receiver<DapEvent>,
    crossbeam_channel::Sender<DapEvent>,
    DapDebugHandle,
) {
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
    let (event_tx, event_rx) = crossbeam_channel::unbounded();
    let event_tx_clone = event_tx.clone();
    let handle = DapDebugHandle {
        event_tx,
        command_rx: cmd_rx,
    };
    (cmd_tx, event_rx, event_tx_clone, handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_channel_protocol_round_trip() {
        let (cmd_tx, evt_rx, evt_tx, handle) = create_dap_channels();
        let cmd_rx = handle.command_rx;

        let vm_thread = thread::spawn(move || {
            let evt = evt_rx.recv().unwrap();
            match &evt {
                DapEvent::Stopped {
                    reason,
                    vm_state: state,
                } => {
                    assert!(matches!(reason, StopReason::Step));
                    assert_eq!(state.function_name, "test_module::test_fn::0");
                    assert_eq!(state.pc, 0);
                },
                _ => panic!("expected Stopped event"),
            }
            cmd_tx.send(DapCommand::Step(1)).unwrap();

            let evt = evt_rx.recv().unwrap();
            assert!(matches!(evt, DapEvent::Stopped { .. }));
            cmd_tx.send(DapCommand::Continue).unwrap();

            let evt = evt_rx.recv().unwrap();
            assert!(matches!(evt, DapEvent::Terminated { .. }));
        });

        evt_tx
            .send(DapEvent::Stopped {
                reason: StopReason::Step,
                vm_state: VmStoppedState {
                    function_name: "test_module::test_fn::0".to_string(),
                    pc: 0,
                    instruction: "Call(0)".to_string(),
                    dap_stack_trace: vec![DapFrameInfo {
                        function_name: "test_module::test_fn".to_string(),
                        pc: 0,
                        source_location: Some("test.move:10".to_string()),
                        locals: vec![],
                    }],
                    dap_locals: vec![DapLocalInfo {
                        index: 0,
                        name: "x".to_string(),
                        type_name: "u64".to_string(),
                        value: DebugValue::Primitive("42".to_string()),
                    }],
                    source_location: Some("test.move:10".to_string()),
                },
            })
            .unwrap();

        let cmd = cmd_rx.recv().unwrap();
        assert!(matches!(cmd, DapCommand::Step(1)));

        evt_tx
            .send(DapEvent::Stopped {
                reason: StopReason::Breakpoint("test_module::test_fn".to_string()),
                vm_state: VmStoppedState {
                    function_name: "test_module::test_fn::1".to_string(),
                    pc: 1,
                    instruction: "Ret".to_string(),
                    dap_stack_trace: vec![],
                    dap_locals: vec![],
                    source_location: None,
                },
            })
            .unwrap();

        let cmd = cmd_rx.recv().unwrap();
        assert!(matches!(cmd, DapCommand::Continue));

        evt_tx.send(DapEvent::Terminated { message: None }).unwrap();

        vm_thread.join().unwrap();
    }
}
