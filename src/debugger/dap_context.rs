use crate::debugger::debug_value::DebugValue;
use crate::debugger::{
    dap_types::{
        DapCommand, DapDebugHandle, DapEvent, DapFrameInfo, DapLocalInfo, StopReason,
        VmStoppedState,
    },
    resolver::{self, LocatorTypeResolver},
};
use move_vm_runtime::{
    LoadedFunction, RuntimeEnvironment,
    debug::{DebugContext, InterpreterDebugInterface, ThreadStateHandle},
    source_locator, tracing,
};
use move_vm_types::{instr::Instruction, values::Locals};
use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};

#[derive(Debug)]
enum DebuggerOp {
    StepOverLine {
        stack_depth: usize,
        start_source_loc: Option<String>,
    },
    StepRemaining(usize),
    StepOut {
        target_stack_depth: usize,
    },
    Continue,
}

pub struct DapDebugContext {
    event_tx: crossbeam_channel::Sender<DapEvent>,
    command_rx: crossbeam_channel::Receiver<DapCommand>,
    current_op: DebuggerOp,
    breakpoints: BTreeSet<String>,
    moved_locals: HashMap<usize, HashMap<usize, DebugValue>>,
    last_breakpoint_sloc: Option<(String, usize)>,
}

impl DapDebugContext {
    pub fn new(handle: DapDebugHandle) -> Self {
        Self {
            event_tx: handle.event_tx,
            command_rx: handle.command_rx,
            current_op: DebuggerOp::StepRemaining(1),
            breakpoints: BTreeSet::new(),
            moved_locals: HashMap::new(),
            last_breakpoint_sloc: None,
        }
    }

    pub fn dap_handle(&self) -> DapDebugHandle {
        DapDebugHandle {
            event_tx: self.event_tx.clone(),
            command_rx: self.command_rx.clone(),
        }
    }

    fn apply_dap_command_queue(
        &mut self,
        function: &LoadedFunction,
        locals: &Locals,
        pc: u16,
        _instr: &Instruction,
        runtime_environment: &RuntimeEnvironment,
        interpreter: &dyn InterpreterDebugInterface,
        instr_string: &str,
        function_string: &str,
        stop_reason: StopReason,
        source_loc: &Option<String>,
    ) {
        let vm_stopped_state = build_vm_stopped_state(
            function,
            locals,
            pc,
            instr_string,
            function_string,
            runtime_environment,
            interpreter,
            &self.moved_locals,
        );

        if self
            .event_tx
            .send(DapEvent::Stopped {
                reason: stop_reason,
                vm_state: vm_stopped_state,
            })
            .is_err()
        {
            self.current_op = DebuggerOp::Continue;
            return;
        }

        loop {
            let cmd = match self.command_rx.recv() {
                Ok(cmd) => cmd,
                Err(_) => {
                    self.current_op = DebuggerOp::Continue;
                    return;
                }
            };
            match cmd {
                DapCommand::Continue => {
                    self.current_op = DebuggerOp::Continue;
                    break;
                }
                DapCommand::Step(n) => {
                    self.current_op = DebuggerOp::StepRemaining(n);
                    break;
                }
                DapCommand::StepOver(_) => {
                    self.current_op = DebuggerOp::StepOverLine {
                        stack_depth: interpreter.get_stack_depth(),
                        start_source_loc: source_loc.clone(),
                    };
                    break;
                }
                DapCommand::StepOut => {
                    let stack_depth = interpreter.get_stack_depth();
                    if stack_depth == 0 {
                        self.current_op = DebuggerOp::Continue;
                    } else {
                        self.current_op = DebuggerOp::StepOut {
                            target_stack_depth: stack_depth - 1,
                        };
                    }
                    break;
                }
                DapCommand::SetBreakpoints(bps) => {
                    self.breakpoints = bps.into_iter().collect();
                }
            }
        }
    }
}

impl DebugContext for DapDebugContext {
    fn debug_loop(
        &mut self,
        function: &LoadedFunction,
        locals: &Locals,
        pc: u16,
        instr: &Instruction,
        runtime_environment: &RuntimeEnvironment,
        interpreter: &dyn InterpreterDebugInterface,
    ) {
        let current_stack_depth = interpreter.get_stack_depth();

        match instr {
            Instruction::MoveLoc(idx) => {
                let idx = *idx as usize;
                if let Some(ty) = function.local_tys().get(idx) {
                    let resolver = LocatorTypeResolver::new(runtime_environment, interpreter);
                    let sv =
                        crate::debugger::debug_value::serialize_value_for_debug(locals, idx, ty, &resolver);
                    self.moved_locals
                        .entry(current_stack_depth)
                        .or_default()
                        .insert(idx, sv);
                }
            }
            Instruction::Ret => {
                self.moved_locals.remove(&current_stack_depth);
            }
            _ => {}
        }

        let instr_string = format!("{:?}", instr);
        let function_string = function.name_as_pretty_string();
        let current_sloc = function.module_id().and_then(|mid| {
            source_locator::get_bytecode_source_location(mid, function.index(), pc)
        });

        if let Some((ref prev_bp_sloc, prev_bp_depth)) = self.last_breakpoint_sloc {
            if current_stack_depth <= prev_bp_depth
                && current_sloc.as_deref() != Some(prev_bp_sloc.as_str())
            {
                self.last_breakpoint_sloc = None;
            }
        }
        let is_under_the_same_bp_sloc = match (&current_sloc, &self.last_breakpoint_sloc) {
            (Some(loc), Some((prev_bp_sloc, prev_bp_depth))) => {
                loc == prev_bp_sloc && current_stack_depth == *prev_bp_depth
            }
            _ => false,
        };
        let breakpoint_hit = !is_under_the_same_bp_sloc
            && self.breakpoints.iter().any(|bp| {
                instr_string[..].starts_with(bp.as_str())
                    || current_sloc.as_deref() == Some(bp.as_str())
            });

        let should_take_input = match &mut self.current_op {
            DebuggerOp::StepRemaining(n) => {
                if *n == 1 {
                    self.current_op = DebuggerOp::Continue;
                    true
                } else {
                    *n -= 1;
                    false
                }
            }
            DebuggerOp::StepOverLine {
                stack_depth,
                start_source_loc,
            } => {
                let current_depth = interpreter.get_stack_depth();
                if *stack_depth >= current_depth {
                    let line_changed = match (&current_sloc, &*start_source_loc) {
                        (Some(cur), Some(start)) => cur != start,
                        (Some(_), None) => true,
                        _ => false,
                    };
                    if line_changed {
                        self.current_op = DebuggerOp::Continue;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            DebuggerOp::StepOut { target_stack_depth } => {
                if *target_stack_depth == interpreter.get_stack_depth() {
                    self.current_op = DebuggerOp::Continue;
                    true
                } else {
                    false
                }
            }
            DebuggerOp::Continue => false,
        };

        if !should_take_input && !breakpoint_hit {
            return;
        }

        let stop_reason = if breakpoint_hit {
            self.last_breakpoint_sloc = current_sloc
                .as_ref()
                .map(|loc| (loc.clone(), current_stack_depth));
            let bp_match = self
                .breakpoints
                .iter()
                .find(|bp| {
                    instr_string.starts_with(bp.as_str())
                        || current_sloc.as_deref() == Some(bp.as_str())
                })
                .cloned()
                .unwrap_or(function_string.clone());
            StopReason::Breakpoint(bp_match)
        } else {
            StopReason::Step
        };

        self.apply_dap_command_queue(
            function,
            locals,
            pc,
            instr,
            runtime_environment,
            interpreter,
            &instr_string,
            &function_string,
            stop_reason,
            &current_sloc,
        );
    }

    fn capture_thread_state(&self) -> Box<dyn ThreadStateHandle> {
        Box::new(DapThreadState {
            dap_handle: self.dap_handle(),
            source_locator: source_locator::get_source_locator(),
        })
    }
}

struct DapThreadState {
    dap_handle: DapDebugHandle,
    source_locator: Option<Arc<dyn source_locator::SourceLocator>>,
}

impl ThreadStateHandle for DapThreadState {
    fn install_on_thread(&self) {
        tracing::set_debugging_enabled(true);
        if let Some(loc) = &self.source_locator {
            source_locator::set_source_locator(loc.clone());
        }
        tracing::set_debug_context(Box::new(DapDebugContext::new(self.dap_handle.clone())));
    }
}

fn build_dap_local_infos(
    function: &LoadedFunction,
    locals: &Locals,
    runtime_environment: &RuntimeEnvironment,
    interpreter: &dyn InterpreterDebugInterface,
    moved_locals: Option<&HashMap<usize, DebugValue>>,
) -> Vec<DapLocalInfo> {
    if function.local_tys().is_empty() {
        return vec![];
    }
    let local_infos = resolver::build_local_infos(function);
    let name_resolver = LocatorTypeResolver::new(runtime_environment, interpreter);

    local_infos
        .into_iter()
        .map(|local_info| {
            let ty = &function.local_tys()[local_info.index];
            let debug_value = crate::debugger::debug_value::serialize_value_for_debug(
                locals,
                local_info.index,
                ty,
                &name_resolver,
            );
            let debug_value = if matches!(&debug_value, DebugValue::Invalid) {
                moved_locals
                    .and_then(|m| m.get(&local_info.index))
                    .cloned()
                    .unwrap_or(debug_value)
            } else {
                debug_value
            };
            DapLocalInfo {
                index: local_info.index,
                name: local_info.name,
                type_name: String::new(),
                value: debug_value,
            }
        })
        .collect()
}

fn build_vm_stopped_state(
    function: &LoadedFunction,
    locals: &Locals,
    pc: u16,
    instr_string: &str,
    function_string: &str,
    runtime_environment: &RuntimeEnvironment,
    interpreter: &dyn InterpreterDebugInterface,
    moved_locals: &HashMap<usize, HashMap<usize, DebugValue>>,
) -> VmStoppedState {
    let source_location = function.module_id().and_then(|module_id| {
        source_locator::get_bytecode_source_location(module_id, function.index(), pc)
    });

    let stack_depth = interpreter.get_stack_depth();
    let dap_stack_trace = interpreter
        .get_stack_frames(usize::MAX)
        .stack_trace()
        .iter()
        .enumerate()
        .map(|(i, (module_id, func_def_idx, code_offset))| {
            let frame_source_loc = module_id.as_ref().and_then(|mid| {
                source_locator::get_bytecode_source_location(mid, *func_def_idx, *code_offset)
            });
            let caller_depth = stack_depth - 1 - i;
            let moved_locals_at_frame = moved_locals.get(&caller_depth);
            let (frame_fname, frame_locals_infos) = match interpreter.get_frame_locals(i) {
                Some((func, locs)) => (
                    func.name_as_pretty_string(),
                    build_dap_local_infos(
                        func,
                        locs,
                        runtime_environment,
                        interpreter,
                        moved_locals_at_frame,
                    ),
                ),
                None => {
                    let name = module_id
                        .as_ref()
                        .map(|mid| format!("{}::{}", mid, func_def_idx))
                        .unwrap_or_else(|| format!("<script>::{}", func_def_idx));
                    (name, vec![])
                }
            };
            DapFrameInfo {
                function_name: frame_fname,
                pc: *code_offset,
                source_location: frame_source_loc,
                locals: frame_locals_infos,
            }
        })
        .collect();

    let current_moved_locals = moved_locals.get(&stack_depth);
    let local_infos = build_dap_local_infos(
        function,
        locals,
        runtime_environment,
        interpreter,
        current_moved_locals,
    );

    VmStoppedState {
        function_name: function_string.to_string(),
        pc,
        instruction: instr_string.to_string(),
        dap_stack_trace,
        dap_locals: local_infos,
        source_location,
    }
}
