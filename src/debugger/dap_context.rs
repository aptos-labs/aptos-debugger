use crate::debugger::debug_value::DebugValue;
use crate::debugger::{
    dap_types::{
        DapCommand, DapDebugHandle, DapEvent, DapFrameInfo, DapLocalInfo, StopReason,
        VmStoppedState,
    },
    debug_value,
    resolver::{self, LocatorTypeResolver},
};
use move_vm_runtime::{
    debug::{DebugContext, InterpreterDebugInterface, ThreadStateHandle}, source_locator,
    tracing,
    LoadedFunction, RuntimeEnvironment,
};
use move_vm_types::{instr::Instruction, values::Locals};
use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};

#[derive(Debug)]
enum DebuggerOp {
    StepOver {
        line_stack_depth: usize,
        line_sloc: Option<String>,
    },
    StepInto {
        line_sloc: Option<String>,
    },
    StepOut {
        target_stack_depth: usize,
    },
    RunUntilBreakpoint,
}

pub struct DapDebugContext {
    event_tx: crossbeam_channel::Sender<DapEvent>,
    command_rx: crossbeam_channel::Receiver<DapCommand>,
    next_cmd_op: DebuggerOp,
    breakpoints: BTreeSet<String>,
    moved_locals: HashMap<usize, HashMap<usize, DebugValue>>,
    // (loc, stack depth)
    last_breakpoint_hit: Option<(String, usize)>,
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

impl DebugContext for DapDebugContext {
    /// Executed before each bytecode instruction by the VM.
    fn debug_loop(
        &mut self,
        function: &LoadedFunction,
        locals: &Locals,
        pc: u16,
        instr: &Instruction,
        runtime_environment: &RuntimeEnvironment,
        interpreter: &dyn InterpreterDebugInterface,
    ) {
        self.handle_potentially_moved_locals(
            function,
            locals,
            instr,
            runtime_environment,
            interpreter,
        );

        let current_line = function.module_id().and_then(|mid| {
            source_locator::get_bytecode_source_location(mid, function.index(), pc)
        });
        let current_stack_depth = interpreter.get_stack_depth();

        // should be checked before `self.next_cmd_op` to not stop at the line twice
        let breakpoint_hit =
            self.check_if_breakpoint_got_hit(current_stack_depth, current_line.clone());

        let should_stop_at_current_line =
            self.should_stop_for_the_next_cmd_op(interpreter, current_line.clone());

        if !should_stop_at_current_line && breakpoint_hit.is_none() {
            return;
        }

        let vm_stopped_state = build_vm_stopped_state(
            function,
            locals,
            runtime_environment,
            interpreter,
            current_line.clone(),
            &self.moved_locals,
        );
        let stop_reason = match breakpoint_hit {
            Some(breakpoint_hit) => StopReason::Breakpoint(breakpoint_hit),
            None => StopReason::Step,
        };

        // send VM state
        if self
            .event_tx
            .send(DapEvent::Stopped {
                reason: stop_reason,
                vm_state: vm_stopped_state,
            })
            .is_err()
        {
            self.next_cmd_op = DebuggerOp::RunUntilBreakpoint;
            return;
        }

        loop {
            // blocks until session sends another debugger command
            let cmd = match self.command_rx.recv() {
                Ok(cmd) => cmd,
                Err(_) => {
                    self.next_cmd_op = DebuggerOp::RunUntilBreakpoint;
                    return;
                }
            };
            if let DapCommand::SetBreakpoints(bps) = cmd {
                self.breakpoints = bps.into_iter().collect();
                continue;
            }
            self.next_cmd_op = parse_cmd_op(cmd, current_stack_depth, current_line);
            break;
        }
    }

    fn capture_thread_state(&self) -> Box<dyn ThreadStateHandle> {
        Box::new(DapThreadState {
            dap_handle: DapDebugHandle {
                event_tx: self.event_tx.clone(),
                command_rx: self.command_rx.clone(),
            },
            source_locator: source_locator::get_source_locator(),
        })
    }
}

impl DapDebugContext {
    pub fn new(handle: DapDebugHandle) -> Self {
        Self {
            event_tx: handle.event_tx,
            command_rx: handle.command_rx,
            next_cmd_op: DebuggerOp::StepInto { line_sloc: None },
            breakpoints: BTreeSet::new(),
            moved_locals: HashMap::new(),
            last_breakpoint_hit: None,
        }
    }

    /// After each variable is "moved" from scope throughout the execution, it disappears from Locals.
    /// We still want to be able to inspect it in the Variables view, so we save all of those for later in `self.moved_locals`.
    fn handle_potentially_moved_locals(
        &mut self,
        function: &LoadedFunction,
        locals: &Locals,
        instr: &Instruction,
        runtime_environment: &RuntimeEnvironment,
        interpreter: &dyn InterpreterDebugInterface,
    ) {
        match instr {
            Instruction::MoveLoc(local_idx) => {
                let local_idx = *local_idx as usize;
                if let Some(local_ty) = function.local_tys().get(local_idx) {
                    let type_resolver = LocatorTypeResolver::new(runtime_environment, interpreter);
                    let dv = debug_value::serialize_local_value(
                        locals,
                        local_idx,
                        local_ty,
                        &type_resolver,
                    );
                    // we save locals only on specific `current_stack_depth`, and clear those on `Ret` later
                    self.moved_locals
                        .entry(interpreter.get_stack_depth())
                        .or_default()
                        .insert(local_idx, dv);
                }
            }
            Instruction::Ret => {
                self.moved_locals.remove(&interpreter.get_stack_depth());
            }
            _ => (),
        }
    }

    fn check_if_breakpoint_got_hit(
        &mut self,
        current_stack_depth: usize,
        current_line: Option<String>,
    ) -> Option<String> {
        // Suppress re-triggering the same breakpoint on consecutive bytecode instructions
        // that map to the same source line. Clears `last_breakpoint_loc` once we've moved
        // past it (different line at same/shallower depth), re-enabling it for future hits.
        let bp_suppressed = match &self.last_breakpoint_hit {
            // deeper in the stack — don't clear, don't suppress
            Some((_, last_bp_depth)) if current_stack_depth > *last_bp_depth => false,
            // on the same bp line, suppress it if we're on the same depth
            Some((last_bp_line, last_bp_depth))
                if current_line.as_deref() == Some(last_bp_line.as_str()) =>
            {
                current_stack_depth == *last_bp_depth
            }
            // moved to a different line at the acceptable stack depth, so bp shouldn't be suppressed.
            // clear it for later usage too (i.e. in loops)
            Some(_) => {
                self.last_breakpoint_hit = None;
                false
            }
            None => false,
        };
        if bp_suppressed {
            return None;
        }

        let breakpoint_hit = self
            .breakpoints
            .iter()
            .find(|bp| current_line.as_deref() == Some(bp.as_str()))
            .cloned();

        if let Some(breakpoint_hit) = breakpoint_hit.clone() {
            self.last_breakpoint_hit = Some((breakpoint_hit, current_stack_depth));
        }

        breakpoint_hit
    }

    fn should_stop_for_the_next_cmd_op(
        &mut self,
        interpreter: &dyn InterpreterDebugInterface,
        current_source_line: Option<String>,
    ) -> bool {
        let current_stack_depth = interpreter.get_stack_depth();
        let should_stop_after_op = match &self.next_cmd_op {
            DebuggerOp::StepOver {
                line_stack_depth,
                line_sloc,
            } => {
                if *line_stack_depth >= current_stack_depth {
                    let line_changed = match (&current_source_line, &*line_sloc) {
                        (Some(cur), Some(start)) => cur != start,
                        (Some(_), None) => true,
                        _ => false,
                    };
                    line_changed
                } else {
                    false
                }
            }
            DebuggerOp::StepInto { line_sloc } => {
                match (&current_source_line, &*line_sloc) {
                    (Some(cur), Some(start)) => cur != start,
                    (Some(_), None) => true,
                    _ => false,
                }
            }
            // stop if we out of the target stack depth
            DebuggerOp::StepOut { target_stack_depth } => {
                current_stack_depth <= *target_stack_depth
            }
            DebuggerOp::RunUntilBreakpoint => false,
        };
        // next command after stop is already `DebuggerOp::RunUntilBreakpoint`
        if should_stop_after_op {
            self.next_cmd_op = DebuggerOp::RunUntilBreakpoint;
        }
        should_stop_after_op
    }
}

fn parse_cmd_op(
    cmd: DapCommand,
    line_stack_depth: usize,
    current_line: Option<String>,
) -> DebuggerOp {
    match cmd {
        DapCommand::Continue => DebuggerOp::RunUntilBreakpoint,
        DapCommand::StepInto => DebuggerOp::StepInto {
            line_sloc: current_line.clone(),
        },
        DapCommand::StepOver => DebuggerOp::StepOver {
            line_stack_depth,
            line_sloc: current_line.clone(),
        },
        DapCommand::StepOut => {
            let stack_depth = line_stack_depth;
            if stack_depth == 0 {
                DebuggerOp::RunUntilBreakpoint
            } else {
                DebuggerOp::StepOut {
                    target_stack_depth: stack_depth - 1,
                }
            }
        }
        DapCommand::SetBreakpoints(_) => unreachable!(),
    }
}

fn build_dap_local_infos(
    function: &LoadedFunction,
    locals: &Locals,
    runtime_environment: &RuntimeEnvironment,
    interpreter: &dyn InterpreterDebugInterface,
    moved_locals: Option<&HashMap<usize, DebugValue>>,
) -> Vec<DapLocalInfo> {
    let local_infos = resolver::build_local_infos(function);
    let type_resolver = LocatorTypeResolver::new(runtime_environment, interpreter);

    local_infos
        .into_iter()
        .map(|local_info| {
            let debug_value = debug_value::serialize_local_value(
                locals,
                local_info.index,
                &local_info.ty,
                &type_resolver,
            );
            let debug_value = match debug_value {
                DebugValue::Invalid => moved_locals
                    .and_then(|m| m.get(&local_info.index))
                    .cloned()
                    .unwrap_or(debug_value),
                _ => debug_value,
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
    runtime_environment: &RuntimeEnvironment,
    interpreter: &dyn InterpreterDebugInterface,
    current_line: Option<String>,
    moved_locals: &HashMap<usize, HashMap<usize, DebugValue>>,
) -> VmStoppedState {
    let stack_depth = interpreter.get_stack_depth();
    let dap_stack_trace = interpreter
        .get_stack_frames(usize::MAX)
        .stack_trace()
        .iter()
        .enumerate()
        .map(|(i, (module_id, func_def_idx, code_offset))| {
            let frame_source_line = module_id.as_ref().and_then(|mid| {
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
                source_location: frame_source_line,
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
        function_name: function.name_as_pretty_string(),
        dap_stack_trace,
        dap_locals: local_infos,
        source_location: current_line,
    }
}
