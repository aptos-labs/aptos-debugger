use crate::{proto, server::DapServer};
use anyhow::bail;
use aptos_move_cli::source_locator::AptosSourceLocator;
use aptos_move_debugger::aptos_debugger::AptosDebugger;
use aptos_resource_viewer::module_view::CachedModuleView;
use aptos_rest_client::{AptosBaseUrl, Client};
use aptos_types::transaction::{
    PersistedAuxiliaryInfo, SignedTransaction, Transaction, TransactionInfo, TransactionPayload,
};
use aptos_validator_interface::LocalModuleOverrides;
use dap::types::Variable;
use move_resource_viewer::MoveValueAnnotator;
use move_vm_debugger::{DapDebugContext, DapEvent, DebugValue, create_dap_channels};
use move_vm_runtime::{source_locator, tracing};
use std::{collections::BTreeMap, io, path::PathBuf, sync::Arc, thread};
use url::Url;

pub(crate) const SCOPE_TRANSACTION_INFO: i64 = 1;

pub struct ReplayTransactionSession {
    pub debugger: Arc<AptosDebugger>,
    pub txn_id: u64,
    pub txn: SignedTransaction,
    pub txn_info: TransactionInfo,
    pub aux_info: PersistedAuxiliaryInfo,
    decoded_args: Vec<(String, DebugValue)>,
}

impl ReplayTransactionSession {
    pub async fn create(network: &str, txn_id: u64) -> anyhow::Result<Self> {
        let base_url = match network {
            "mainnet" => AptosBaseUrl::Mainnet,
            "testnet" => AptosBaseUrl::Testnet,
            "devnet" => AptosBaseUrl::Devnet,
            other => AptosBaseUrl::Custom(Url::parse(other)?),
        };
        let client = Client::builder(base_url).build();
        let debugger = AptosDebugger::rest_client(client)?;

        let (txn, txn_info, aux_info) = debugger
            .get_committed_transaction_at_version(txn_id)
            .await?;

        let txn = match txn {
            Transaction::UserTransaction(txn) => txn,
            _ => bail!("only user transactions are supported for debugging"),
        };

        let debugger = Arc::new(debugger);
        let decoded_args = decode_entry_function_args(&debugger, txn_id, &txn);
        Ok(Self {
            debugger,
            txn_id,
            txn,
            txn_info,
            aux_info,
            decoded_args,
        })
    }
}

impl<R: io::Read, W: io::Write> DapServer<R, W> {
    pub(super) fn start_replay_execution(
        &mut self,
        local_packages: Vec<PathBuf>,
        prebuilt_packages: Vec<PathBuf>,
        named_addresses: BTreeMap<String, aptos_types::account_address::AccountAddress>,
        skip_fetch_latest_git_deps: bool,
    ) -> anyhow::Result<()> {
        use legacy_move_compiler::compiled_unit::CompiledUnit;

        let session = self
            .txn_session
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no session"))?;
        let txn_id = session.txn_id;
        let txn = session.txn.clone();
        let aux_info = session.aux_info;

        let mut module_overrides = LocalModuleOverrides::new();
        let mut locator = AptosSourceLocator::new();

        for pkg_path in &local_packages {
            self.send_console(format_args!(
                "aptos-dap: building local package {}...",
                pkg_path.display()
            ))?;
            let built = aptos_framework::BuiltPackage::build(
                pkg_path.clone(),
                aptos_framework::BuildOptions {
                    with_srcs: true,
                    with_source_maps: true,
                    skip_fetch_latest_git_deps,
                    forced_named_addresses: named_addresses.clone(),
                    ..aptos_framework::BuildOptions::default()
                },
            )
            .map_err(|e| {
                anyhow::anyhow!(
                    "failed to build local package at {}: {}",
                    pkg_path.display(),
                    e
                )
            })?;

            for unit in built.package.root_modules() {
                if let CompiledUnit::Module(ref named) = unit.unit {
                    let module = &named.module;
                    let mut bytes = vec![];
                    module.serialize(&mut bytes)?;
                    let module_id = module.self_id();
                    module_overrides.add_module(&module_id, bytes);

                    let sm_bytes = unit.unit.serialize_source_map();
                    let source_text =
                        std::fs::read_to_string(&unit.source_path).unwrap_or_default();
                    let filename = unit
                        .source_path
                        .canonicalize()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| unit.source_path.to_string_lossy().into_owned());
                    if let Err(e) =
                        locator.add_local_module(module, &sm_bytes, &source_text, &filename)
                    {
                        self.send_console(format_args!(
                            "aptos-dap: could not load source map for {}: {}",
                            module_id, e
                        ))?;
                    }
                }
            }
        }

        for build_dir in &prebuilt_packages {
            self.send_console(format_args!(
                "aptos-dap: loading prebuilt package {}...",
                build_dir.display()
            ))?;
            load_prebuilt_package(build_dir, &mut module_overrides, &mut locator)?;
        }

        let known_files: Vec<String> = locator
            .known_source_files()
            .into_iter()
            .map(|s| s.to_owned())
            .collect();
        let overrides = Arc::new(module_overrides);
        let source_locator: Option<Arc<dyn source_locator::SourceLocator>> =
            if known_files.is_empty() {
                None
            } else {
                Some(Arc::new(locator))
            };

        self.warn_on_unreachable_breakpoints(&known_files)?;

        let (cmd_tx, event_rx, event_tx, handle) = create_dap_channels();

        if source_locator.is_some() {
            self.send_console("aptos-dap: installing source locator")?;
        } else {
            self.send_console("aptos-dap: no source locator (no local packages)")?;
        }

        let debugger = self
            .txn_session
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no session"))?
            .debugger
            .clone();
        let rt_handle = self.rt.handle().clone();

        let vm_handle = thread::spawn(move || -> anyhow::Result<()> {
            tracing::set_debugging_enabled(true);
            tracing::set_debug_context(Box::new(DapDebugContext::new(handle)));
            if let Some(loc) = source_locator {
                source_locator::set_source_locator(loc);
            }
            let _guard = rt_handle.enter();
            eprintln!("aptos-dap: starting transaction execution...");

            let result = aptos_move_cli::local_simulation::run_transaction_with_local_overrides(
                &*debugger, txn_id, txn, aux_info, overrides,
                // source_locator is already installed on thread, pass None here
                None,
            );

            let message = match result {
                Ok((status, _output)) => {
                    eprintln!("aptos-dap: execution finished with status: {:?}", status);
                    None
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    eprintln!("aptos-dap: execution error: {msg}");
                    Some(msg)
                }
            };
            let _ = event_tx.send(DapEvent::Terminated { message });
            Ok(())
        });

        self.cmd_tx = Some(cmd_tx);
        self.event_rx = Some(event_rx);
        self.vm_thread = Some(vm_handle);
        Ok(())
    }

    pub(crate) fn transaction_info_variables(&mut self) -> Vec<Variable> {
        let Some(txn_session) = &self.txn_session else {
            return vec![];
        };
        let mut vars = transaction_info_variables_static(txn_session);
        if !txn_session.decoded_args.is_empty() {
            let count = txn_session.decoded_args.len();
            let arg_fields = txn_session.decoded_args.clone();
            let ref_id = self.stored_variables.store_expandable(DebugValue::Struct {
                name: None,
                ty_args: vec![],
                fields: arg_fields,
            });
            vars.push(Variable {
                variables_reference: ref_id,
                ..proto::var("args", format!("{count} args"))
            });
        }
        vars
    }
}

fn load_prebuilt_package(
    build_dir: &std::path::Path,
    overrides: &mut LocalModuleOverrides,
    locator: &mut AptosSourceLocator,
) -> anyhow::Result<()> {
    use move_binary_format::CompiledModule;

    let bytecode_dir = build_dir.join("bytecode_modules");
    let source_maps_dir = build_dir.join("source_maps");
    let sources_dir = build_dir.join("sources");

    for entry in std::fs::read_dir(&bytecode_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("mv") {
            continue;
        }
        let stem = path.file_stem().unwrap().to_string_lossy();

        let bytes = std::fs::read(&path)?;
        let module = CompiledModule::deserialize(&bytes)
            .map_err(|e| anyhow::anyhow!("failed to deserialize {}: {e}", path.display()))?;
        let module_id = module.self_id();
        overrides.add_module(&module_id, bytes);

        let sm_path = source_maps_dir.join(format!("{stem}.mvsm"));
        let src_path = sources_dir.join(format!("{stem}.move"));
        if sm_path.exists() && src_path.exists() {
            let sm_bytes = std::fs::read(&sm_path)?;
            let source_text = std::fs::read_to_string(&src_path)?;
            let filename = src_path
                .canonicalize()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| src_path.to_string_lossy().into_owned());
            if let Err(e) = locator.add_local_module(&module, &sm_bytes, &source_text, &filename) {
                eprintln!(
                    "aptos-dap: could not load source map for {}: {e}",
                    module_id
                );
            }
        }
    }
    Ok(())
}

fn transaction_info_variables_static(txn_session: &ReplayTransactionSession) -> Vec<Variable> {
    let txn = &txn_session.txn;
    let info = &txn_session.txn_info;
    vec![
        proto::var("version", txn_session.txn_id.to_string()),
        proto::var("sender", txn.sender().to_hex_literal()),
        proto::var("hash", format!("{}", txn.committed_hash())),
        proto::var("function", entry_function_name(txn.payload())),
        proto::var("gas_used", info.gas_used().to_string()),
        proto::var("gas_unit_price", txn.gas_unit_price().to_string()),
        proto::var("max_gas_amount", txn.max_gas_amount().to_string()),
        proto::var("status", format!("{:?}", info.status())),
    ]
}

fn annotated_to_debug_value(v: &move_resource_viewer::AnnotatedMoveValue) -> DebugValue {
    use move_resource_viewer::AnnotatedMoveValue;
    match v {
        AnnotatedMoveValue::Bool(b) => DebugValue::Primitive(b.to_string()),
        AnnotatedMoveValue::U8(n) => DebugValue::Primitive(n.to_string()),
        AnnotatedMoveValue::U16(n) => DebugValue::Primitive(n.to_string()),
        AnnotatedMoveValue::U32(n) => DebugValue::Primitive(n.to_string()),
        AnnotatedMoveValue::U64(n) => DebugValue::Primitive(n.to_string()),
        AnnotatedMoveValue::U128(n) => DebugValue::Primitive(n.to_string()),
        AnnotatedMoveValue::U256(n) => DebugValue::Primitive(n.to_string()),
        AnnotatedMoveValue::I8(n) => DebugValue::Primitive(n.to_string()),
        AnnotatedMoveValue::I16(n) => DebugValue::Primitive(n.to_string()),
        AnnotatedMoveValue::I32(n) => DebugValue::Primitive(n.to_string()),
        AnnotatedMoveValue::I64(n) => DebugValue::Primitive(n.to_string()),
        AnnotatedMoveValue::I128(n) => DebugValue::Primitive(n.to_string()),
        AnnotatedMoveValue::I256(n) => DebugValue::Primitive(n.to_string()),
        AnnotatedMoveValue::Address(a) => DebugValue::Address(a.to_hex_literal()),
        AnnotatedMoveValue::Bytes(b) => DebugValue::Primitive(hex::encode(b)),
        AnnotatedMoveValue::Vector(_, items) => {
            DebugValue::Vector(items.iter().map(annotated_to_debug_value).collect())
        }
        AnnotatedMoveValue::Struct(s) => {
            let name = Some(s.ty_tag.name.to_string());
            let ty_args = s
                .ty_tag
                .type_args
                .iter()
                .map(|t| t.to_canonical_string())
                .collect();
            let fields = s
                .value
                .iter()
                .map(|(name, val)| (name.to_string(), annotated_to_debug_value(val)))
                .collect();
            DebugValue::Struct {
                name,
                ty_args,
                fields,
            }
        }
        AnnotatedMoveValue::RawStruct(s) => {
            let fields = s
                .field_values
                .iter()
                .enumerate()
                .map(|(i, val)| (format!("[{i}]"), annotated_to_debug_value(val)))
                .collect();
            DebugValue::Struct {
                name: None,
                ty_args: vec![],
                fields,
            }
        }
        AnnotatedMoveValue::Closure(c) => {
            DebugValue::Closure(format!("{}::{}", c.module_id, c.fun_id))
        }
    }
}

fn decode_entry_function_args(
    debugger: &Arc<AptosDebugger>,
    txn_id: u64,
    txn: &SignedTransaction,
) -> Vec<(String, DebugValue)> {
    use aptos_types::transaction::TransactionExecutableRef;

    let Some(entry_fn) = txn.payload().executable_ref().ok().and_then(|e| match e {
        TransactionExecutableRef::EntryFunction(ef) => Some(ef),
        _ => None,
    }) else {
        return vec![];
    };

    let mut args = vec![];
    if !entry_fn.ty_args().is_empty() {
        args.push((
            "type_args".to_string(),
            DebugValue::Primitive(format!("{:?}", entry_fn.ty_args())),
        ));
    }

    let state_view = debugger.state_view_at_version(txn_id);
    let module_viewer = CachedModuleView::new(state_view);
    let annotator = MoveValueAnnotator::new(module_viewer);
    let decoded = annotator.view_function_arguments(
        entry_fn.module(),
        entry_fn.function(),
        entry_fn.ty_args(),
        entry_fn.args(),
    );

    for (i, arg) in entry_fn.args().iter().enumerate() {
        let value = decoded
            .as_ref()
            .ok()
            .and_then(|vals| vals.get(i))
            .map(annotated_to_debug_value)
            .unwrap_or_else(|| DebugValue::Primitive(hex::encode(arg)));
        args.push((format!("arg[{i}]"), value));
    }
    args
}

pub(crate) fn entry_function_name(payload: &TransactionPayload) -> String {
    use aptos_types::transaction::TransactionExecutableRef;
    match payload.executable_ref().ok() {
        Some(TransactionExecutableRef::EntryFunction(ef)) => {
            format!("{}::{}", ef.module(), ef.function())
        }
        _ => format!("{:?}", payload),
    }
}
