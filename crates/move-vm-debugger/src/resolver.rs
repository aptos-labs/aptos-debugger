use crate::debug_value::{AdtInfo, FieldInfo, TypeResolver};
use move_core_types::{identifier::Identifier, language_storage::ModuleId};
use move_vm_runtime::{
    LoadedFunction, RuntimeEnvironment, debug::InterpreterDebugInterface, source_locator,
};
use move_vm_types::loaded_data::runtime_types::Type;

pub struct LocatorTypeResolver<'a> {
    runtime_environment: &'a RuntimeEnvironment,
    interpreter: &'a dyn InterpreterDebugInterface,
}

impl<'a> LocatorTypeResolver<'a> {
    pub fn new(
        runtime_environment: &'a RuntimeEnvironment,
        interpreter: &'a dyn InterpreterDebugInterface,
    ) -> Self {
        Self {
            runtime_environment,
            interpreter,
        }
    }
}

impl TypeResolver for LocatorTypeResolver<'_> {
    fn get_adt_name(&self, ty: &Type) -> Option<(ModuleId, Identifier)> {
        self.runtime_environment.get_struct_name(ty).ok().flatten()
    }

    fn get_adt_info(&self, ty: &Type) -> Option<AdtInfo> {
        let (module_id, struct_name) = self.get_adt_name(ty)?;

        let struct_type = match ty {
            Type::Struct { idx, .. } | Type::StructInstantiation { idx, .. } => {
                self.interpreter.load_struct_type(idx)
            }
            _ => None,
        };

        let enum_variants = source_locator::get_enum_variant_info(&module_id, struct_name.as_str());

        match enum_variants {
            Some(variants) => {
                let adt_variants = variants
                    .into_iter()
                    .enumerate()
                    .map(|(tag, (variant_name, source_names))| {
                        let field_types = struct_type
                            .as_ref()
                            .and_then(|st| st.fields(Some(tag as u16)).ok().map(|f| f.to_vec()));
                        let fields =
                            merge_field_names_and_types(&source_names, field_types.as_deref());
                        (variant_name, fields)
                    })
                    .collect();
                Some(AdtInfo::Enum {
                    variants: adt_variants,
                })
            }
            None => {
                let source_names =
                    source_locator::get_struct_field_names(&module_id, struct_name.as_str())?;

                let field_types =
                    struct_type.and_then(|st| st.fields(None).ok().map(|f| f.to_vec()));
                let fields = merge_field_names_and_types(&source_names, field_types.as_deref());
                Some(AdtInfo::Struct { fields })
            }
        }
    }
}

fn merge_field_names_and_types(
    source_names: &[String],
    field_types: Option<&[(Identifier, Type)]>,
) -> Vec<FieldInfo> {
    source_names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let ty = field_types.and_then(|ft| ft.get(i).map(|(_, t)| t.clone()));
            (name.clone(), ty)
        })
        .collect()
}

pub struct LocalInfo {
    pub index: usize,
    pub name: String,
    pub ty: Type,
}

pub fn build_local_infos(function: &LoadedFunction) -> Vec<LocalInfo> {
    let total = function.local_tys().len();

    let names = function
        .module_id()
        .and_then(|mid| source_locator::get_function_param_and_local_names(mid, function.index()))
        .map(|(_, n)| n);

    (0..total)
        .map(|local_idx| {
            let name = names
                .as_ref()
                .and_then(|n| n.get(local_idx).cloned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("local[{}]", local_idx));
            let ty = function
                .local_tys()
                .get(local_idx)
                .expect("local_idx derived from function.local_tys()");
            LocalInfo {
                index: local_idx,
                name,
                ty: ty.clone(),
            }
        })
        .collect()
}
