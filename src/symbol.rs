use crate::debuginfo::iter::TypeInfoIter;
use crate::debuginfo::{DbgDataType, VarInfo};
use crate::debuginfo::{DebugData, TypeInfo, make_simple_unit_name};

#[derive(Clone)]
pub(crate) struct SymbolInfo<'dbg> {
    pub(crate) name: String,
    pub(crate) address: u64,
    pub(crate) typeinfo: &'dbg TypeInfo,
    pub(crate) unit_idx: usize,
    pub(crate) function_name: &'dbg Option<String>,
    pub(crate) namespaces: &'dbg [String],
    pub(crate) is_unique: bool,
}

struct AdditionalSpec {
    function_name: Option<String>,
    simple_unit_name: Option<String>,
    namespaces: Vec<String>,
}

// find a symbol in the elf_info data structure that was derived from the DWARF debug info in the elf file
pub(crate) fn find_symbol<'a>(
    varname: &str,
    debug_data: &'a DebugData,
) -> Result<SymbolInfo<'a>, String> {
    // Extension seen in files generated by Vector tools:
    // The varname in a symbol link might contain additional information
    // var{Function:FuncName}{CompileUnit:UnitName_c}{Namespace:Global}"
    // This allows variables that occur in multiple files / functions / namespaces to be identified correctly
    let (plain_symbol, additional_spec) = get_additional_spec(varname);

    // split the a2l symbol name: e.g. "motortune.param._0_" -> ["motortune", "param", "_0_"]
    let components = split_symbol_components(plain_symbol);

    // find the symbol in the symbol table
    match find_symbol_from_components(&components, &additional_spec, debug_data) {
        Ok(sym_info) => Ok(SymbolInfo {
            name: plain_symbol.to_owned(),
            ..sym_info
        }),
        Err(find_err) => {
            // it was not found using the given varname; if this is name has a mangled form then try that instead
            if let Some(mangled) = debug_data.demangled_names.get(components[0]) {
                let mut components_mangled = components.clone();
                components_mangled[0] = mangled;
                if let Ok(sym_info) =
                    find_symbol_from_components(&components_mangled, &additional_spec, debug_data)
                {
                    let mangled_varname =
                        mangled.to_owned() + varname.strip_prefix(components[0]).unwrap();
                    return Ok(SymbolInfo {
                        name: mangled_varname,
                        ..sym_info
                    });
                }
            }

            Err(find_err)
        }
    }
}

fn find_symbol_from_components<'a>(
    components: &[&str],
    additional_spec: &Option<AdditionalSpec>,
    debug_data: &'a DebugData,
) -> Result<SymbolInfo<'a>, String> {
    // the first component of the symbol name is the name of the global variable.
    if let Some(varinfo_list) = debug_data.variables.get(components[0]) {
        // somtimes there are several variables with the same name in different files or functions
        // select the best one of them based on the additional_data
        let varinfo = select_varinfo(varinfo_list, additional_spec, debug_data);
        let is_unique = varinfo_list.len() == 1;

        // we also need the type in order to resolve struct members, etc.
        if let Some(vartype) = debug_data.types.get(&varinfo.typeref) {
            // all further components of the symbol name are struct/union members or array indices
            find_membertype(vartype, debug_data, components, 1, varinfo.address).map(
                |(addr, typeinfo)| SymbolInfo {
                    name: "".to_string(),
                    address: addr,
                    typeinfo,
                    unit_idx: varinfo.unit_idx,
                    function_name: &varinfo.function,
                    namespaces: &varinfo.namespaces,
                    is_unique,
                },
            )
        } else {
            // this exists for completeness, but shouldn't happen with a correctly generated elffile
            // if the variable is present in the elffile, then the type should also be present
            if components.len() == 1 {
                Ok(SymbolInfo {
                    name: "".to_string(),
                    address: varinfo.address,
                    typeinfo: &TypeInfo {
                        datatype: DbgDataType::Uint8,
                        name: None,
                        unit_idx: usize::MAX,
                        dbginfo_offset: 0,
                    },
                    unit_idx: varinfo.unit_idx,
                    namespaces: &varinfo.namespaces,
                    function_name: &None,
                    is_unique,
                })
            } else {
                Err(format!(
                    "Remaining portion \"{}\" of \"{}\" could not be matched",
                    components[1..].join("."),
                    components.join(".")
                ))
            }
        }
    } else {
        Err(format!("Symbol \"{}\" does not exist", components[0]))
    }
}

fn select_varinfo<'a>(
    varinfo_list: &'a [VarInfo],
    additional_spec: &Option<AdditionalSpec>,
    debug_data: &DebugData,
) -> &'a VarInfo {
    if let Some(additional_spec) = additional_spec {
        let unit = &additional_spec.simple_unit_name;
        let func = &additional_spec.function_name;
        let ns = &additional_spec.namespaces;
        for vi in varinfo_list {
            if (unit.is_none() || *unit == make_simple_unit_name(debug_data, vi.unit_idx))
                && (func.is_none() || *func == vi.function)
                && *ns == vi.namespaces
            {
                return vi;
            }
        }
        // spec was NOT matched. In this case we simply continue as if the spec didin't exist
    }
    &varinfo_list[0]
}

// split up a string of the form
// var{Function:FuncName}{CompileUnit:UnitName_c}{Namespace:Global}"
fn get_additional_spec(varname_ext: &str) -> (&str, Option<AdditionalSpec>) {
    if let Some(pos) = varname_ext.find('{') {
        let (base, spec_str) = varname_ext.split_at(pos);
        // ex: base = "var", spec_str = "{Function:FuncName}{CompileUnit:UnitName_c}{Namespace:Global}"
        if let Some(spec_str) = spec_str.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            // spec_str = "Function:FuncName}{CompileUnit:UnitName_c}{Namespace:Global"
            let mut add_spec = AdditionalSpec {
                function_name: None,
                simple_unit_name: None,
                namespaces: vec![],
            };
            for component in spec_str.split("}{") {
                // component = "Function:FuncName" / "CompileUnit:UnitName_c" / "Namespace:Global"
                if let Some(func_name) = component.strip_prefix("Function:") {
                    add_spec.function_name = Some(func_name.to_string());
                } else if let Some(nsname) = component.strip_prefix("Namespace:") {
                    add_spec.namespaces.push(nsname.to_string());
                } else if let Some(name) = component.strip_prefix("CompileUnit:") {
                    add_spec.simple_unit_name = Some(name.to_string());
                    // CompileUnit:... is the last interesting entry - skip the final {Namespace:Global}
                    break;
                }
            }

            (base, Some(add_spec))
        } else {
            (base, None)
        }
    } else {
        (varname_ext, None)
    }
}

// split the symbol into components
// e.g. "my_struct.array_field[5][6]" -> [ "my_struct", "array_field", "[5]", "[6]" ]
fn split_symbol_components(varname: &str) -> Vec<&str> {
    let mut components: Vec<&str> = Vec::new();

    for component in varname.split('.') {
        if let Some(idx) = component.find('[') {
            // "array_field[5][6]" -> "array_field", "[5][6]"
            let (name, indexstring) = component.split_at(idx);
            components.push(name);
            components.extend(indexstring.split_inclusive(']'));
        } else {
            components.push(component);
        }
    }

    components
}

// find the address and type of the current component of a symbol name
fn find_membertype<'a>(
    typeinfo: &'a TypeInfo,
    debug_data: &'a DebugData,
    components: &[&str],
    component_index: usize,
    address: u64,
) -> Result<(u64, &'a TypeInfo), String> {
    if component_index >= components.len() {
        Ok((address, typeinfo))
    } else {
        println!("typeinfo.datatype: {:?}", &typeinfo.datatype);
        match &typeinfo.datatype {
            DbgDataType::Class {
                members,
                inheritance,
                ..
            } => {
                if let Some((membertype, offset)) = members.get(components[component_index]) {
                    let membertype = membertype.get_reference(&debug_data.types);
                    find_membertype(
                        membertype,
                        debug_data,
                        components,
                        component_index + 1,
                        address + offset,
                    )
                } else if let Some((baseclass_type, offset)) =
                    inheritance.get(components[component_index])
                {
                    let skip = usize::from(
                        components.len() > component_index + 1
                            && components[component_index + 1] == "_",
                    );
                    find_membertype(
                        baseclass_type,
                        debug_data,
                        components,
                        component_index + 1 + skip,
                        address + offset,
                    )
                } else {
                    Err(format!(
                        "There is no member \"{}\" in \"{}\"",
                        components[component_index],
                        components[..component_index].join(".")
                    ))
                }
            }
            DbgDataType::Struct { members, .. } | DbgDataType::Union { members, .. } => {
                if let Some((membertype, offset)) = members.get(components[component_index]) {
                    let membertype = membertype.get_reference(&debug_data.types);
                    find_membertype(
                        membertype,
                        debug_data,
                        components,
                        component_index + 1,
                        address + offset,
                    )
                } else {
                    Err(format!(
                        "There is no member \"{}\" in \"{}\"",
                        components[component_index],
                        components[..component_index].join(".")
                    ))
                }
            }
            DbgDataType::Array {
                dim,
                stride,
                arraytype,
                ..
            } => {
                let mut multi_index = 0;
                for (idx_pos, current_dim) in dim.iter().enumerate() {
                    let arraycomponent =
                        components.get(component_index + idx_pos).unwrap_or(&"_0_"); // default to first element if no more components are specified
                    let indexval = get_index(arraycomponent).ok_or_else(|| {
                        format!("could not interpret \"{arraycomponent}\" as an array index")
                    })?;
                    if indexval >= *current_dim as usize {
                        return Err(format!(
                            "requested array index {} in expression \"{}\", but the array only has {} elements",
                            indexval,
                            components.join("."),
                            current_dim
                        ));
                    }
                    multi_index = multi_index * (*current_dim) as usize + indexval;
                }

                let elementaddr = address + (multi_index as u64 * stride);
                find_membertype(
                    arraytype,
                    debug_data,
                    components,
                    component_index + dim.len(),
                    elementaddr,
                )
            }
            _ => {
                if component_index >= components.len() {
                    Ok((address, typeinfo))
                } else {
                    // could not descend further to match additional symbol name components

                    Err(format!(
                        "Remaining portion \"{}\" of \"{}\" could not be matched",
                        components[component_index..].join("."),
                        components.join(".")
                    ))
                }
            }
        }
    }
}

// before ASAP2 1.7 array indices in symbol names could not written as [x], but only as _x_
// this function will get the numerical index for either representation
fn get_index(idxstr: &str) -> Option<usize> {
    if (idxstr.starts_with('_') && idxstr.ends_with('_'))
        || (idxstr.starts_with('[') && idxstr.ends_with(']'))
    {
        let idxstrlen = idxstr.len();
        idxstr[1..(idxstrlen - 1)].parse().ok()
    } else {
        None
    }
}

/// find a component of a symbol based on an offset from the base address
/// For example this could be a particular array element or struct member
pub(crate) fn find_symbol_by_offset<'a>(
    base_symbol: &SymbolInfo<'a>,
    offset: i32,
    debug_data: &'a DebugData,
) -> Result<SymbolInfo<'a>, String> {
    if offset < 0 || offset > base_symbol.typeinfo.get_size() as i32 {
        return Err(format!(
            "Offset {} is out of bounds for symbol \"{}\"",
            offset, base_symbol.name
        ));
    }

    let offset = offset as u64;

    let iter = TypeInfoIter::new(&debug_data.types, base_symbol.typeinfo, false);
    for (name, typeinfo, item_offset) in iter {
        if item_offset == offset {
            return Ok(SymbolInfo {
                name: format!("{}{}", base_symbol.name, name),
                address: item_offset + base_symbol.address,
                typeinfo,
                unit_idx: base_symbol.unit_idx,
                function_name: base_symbol.function_name,
                namespaces: base_symbol.namespaces,
                is_unique: base_symbol.is_unique,
            });
        }
    }

    Err(format!(
        "Could not find a symbol component at offset {offset} from \"{}\"",
        base_symbol.name
    ))
}

#[cfg(test)]
mod test {
    use super::*;
    use indexmap::IndexMap;
    use std::collections::HashMap;

    #[test]
    fn test_split_symbol_components() {
        let result = split_symbol_components("my_struct.array_field[5][1]");
        assert_eq!(result.len(), 4);
        assert_eq!(result[0], "my_struct");
        assert_eq!(result[1], "array_field");
        assert_eq!(result[2], "[5]");
        assert_eq!(result[3], "[1]");

        let result2 = split_symbol_components("my_struct.array_field._5_._1_");
        assert_eq!(result2.len(), 4);
        assert_eq!(result2[0], "my_struct");
        assert_eq!(result2[1], "array_field");
        assert_eq!(result2[2], "_5_");
        assert_eq!(result2[3], "_1_");
    }

    #[test]
    fn test_find_symbol_of_array() {
        let mut dbgdata = DebugData {
            types: HashMap::new(),
            typenames: HashMap::new(),
            variables: IndexMap::new(),
            demangled_names: HashMap::new(),
            unit_names: Vec::new(),
            sections: HashMap::new(),
        };
        // global variable: uint32_t my_array[2]
        dbgdata.variables.insert(
            "my_array".to_string(),
            vec![crate::debuginfo::VarInfo {
                address: 0x1234,
                typeref: 1,
                unit_idx: 0,
                function: None,
                namespaces: vec![],
            }],
        );
        dbgdata.types.insert(
            1,
            TypeInfo {
                datatype: DbgDataType::Array {
                    arraytype: Box::new(TypeInfo {
                        datatype: DbgDataType::Uint32,
                        name: None,
                        unit_idx: usize::MAX,
                        dbginfo_offset: 0,
                    }),
                    dim: vec![2],
                    size: 8, // total size of the array
                    stride: 4,
                },
                name: None,
                unit_idx: usize::MAX,
                dbginfo_offset: 0,
            },
        );

        // try the different array indexing notations
        let result1 = find_symbol("my_array._0_", &dbgdata);
        assert!(result1.is_ok());
        // C-style notation is only allowed starting with ASAP2 version 1.7, before that the '[' and ']' are not allowed in names
        let result2 = find_symbol("my_array[0]", &dbgdata);
        assert!(result2.is_ok());

        // it should also be possible to get a typeref for the entire array
        let result3 = find_symbol("my_array", &dbgdata);
        assert!(result3.is_ok());

        // there should not be a result if the symbol name contains extra unmatched components
        let result4 = find_symbol("my_array._0_.lalala", &dbgdata);
        assert!(result4.is_err());
        // going past the end of the array is also not permitted
        let result5 = find_symbol("my_array._2_", &dbgdata);
        assert!(result5.is_err());
    }

    #[test]
    fn test_find_symbol_of_array_in_struct() {
        let mut dbgdata = DebugData {
            types: HashMap::new(),
            typenames: HashMap::new(),
            variables: IndexMap::new(),
            demangled_names: HashMap::new(),
            unit_names: Vec::new(),
            sections: HashMap::new(),
        };
        // global variable defined in C like this:
        // struct {
        //        uint32_t array_item[2];
        // } my_struct;
        let mut structmembers: IndexMap<String, (TypeInfo, u64)> = IndexMap::new();
        structmembers.insert(
            "array_item".to_string(),
            (
                TypeInfo {
                    datatype: DbgDataType::Array {
                        arraytype: Box::new(TypeInfo {
                            datatype: DbgDataType::Uint32,
                            name: None,
                            unit_idx: usize::MAX,
                            dbginfo_offset: 0,
                        }),
                        dim: vec![2],
                        size: 8,
                        stride: 4,
                    },
                    name: None,
                    unit_idx: usize::MAX,
                    dbginfo_offset: 0,
                },
                0,
            ),
        );
        dbgdata.variables.insert(
            "my_struct".to_string(),
            vec![crate::debuginfo::VarInfo {
                address: 0x00ca_fe00,
                typeref: 2,
                unit_idx: 0,
                function: None,
                namespaces: vec![],
            }],
        );
        dbgdata.types.insert(
            2,
            TypeInfo {
                datatype: DbgDataType::Struct {
                    members: structmembers,
                    size: 4,
                },
                unit_idx: 0,
                name: None,
                dbginfo_offset: 0,
            },
        );

        // try the different array indexing notations
        let result1 = find_symbol("my_struct.array_item._0_", &dbgdata);
        assert!(result1.is_ok());
        // C-style notation is only allowed starting with ASAP2 version 1.7, before that the '[' and ']' are not allowed in names
        let result2 = find_symbol("my_struct.array_item[0]", &dbgdata);
        assert!(result2.is_ok());

        // theres should not be a result if the symbol name contains extra unmatched components
        let result3 = find_symbol("my_struct.array_item._0_.extra.unused", &dbgdata);
        assert!(result3.is_err());
    }

    #[test]
    fn test_select_varinfo() {
        let mut debug_data = DebugData {
            types: HashMap::new(),
            typenames: HashMap::new(),
            variables: IndexMap::new(),
            demangled_names: HashMap::new(),
            unit_names: Vec::new(),
            sections: HashMap::new(),
        };
        debug_data.types.insert(
            0,
            TypeInfo {
                datatype: DbgDataType::Uint32,
                name: None,
                unit_idx: 0,
                dbginfo_offset: 0,
            },
        );
        debug_data.variables.insert(
            "var".to_string(),
            vec![
                VarInfo {
                    address: 0,
                    typeref: 0,
                    unit_idx: 0,
                    function: Some("func_a".to_string()),
                    namespaces: vec![],
                },
                VarInfo {
                    address: 1000,
                    typeref: 0,
                    unit_idx: 1,
                    function: Some("func_b".to_string()),
                    namespaces: vec![],
                },
                VarInfo {
                    address: 2000,
                    typeref: 0,
                    unit_idx: 1,
                    function: Some("func_c".to_string()),
                    namespaces: vec![],
                },
            ],
        );
        debug_data.unit_names.push(Some("file1.c".to_string()));
        debug_data.unit_names.push(Some("file2.c".to_string()));
        let varinfo_list = debug_data.variables.get("var").unwrap();
        let (base, additional_spec) =
            get_additional_spec("var{Function:func_a}{CompileUnit:file1_c}{Namespace:Global}");
        assert_eq!(base, "var");
        let varinfo = select_varinfo(varinfo_list, &additional_spec, &debug_data);
        assert_eq!(varinfo.address, 0);
        let (base, additional_spec) =
            get_additional_spec("var{Function:func_b}{CompileUnit:file2_c}{Namespace:Global}");
        assert_eq!(base, "var");
        let varinfo = select_varinfo(varinfo_list, &additional_spec, &debug_data);
        assert_eq!(varinfo.address, 1000);
        let (base, additional_spec) =
            get_additional_spec("var{Function:func_c}{CompileUnit:file2_c}{Namespace:Global}");
        assert_eq!(base, "var");
        let varinfo = select_varinfo(varinfo_list, &additional_spec, &debug_data);
        assert_eq!(varinfo.address, 2000);
    }

    #[test]
    fn test_get_additional_spec() {
        let (base, _add_spec) = get_additional_spec("varname");
        assert_eq!(base, "varname");

        let (base, add_spec) = get_additional_spec(
            "varname{Function:func}{Namespace:Foo}{Namespace:Bar}{CompileUnit:file_c}{Namespace:Global}",
        );
        assert_eq!(base, "varname");
        let add_spec = add_spec.unwrap();
        assert_eq!(add_spec.function_name, Some("func".to_string()));
        assert_eq!(add_spec.namespaces, vec!["Foo", "Bar"]);
        assert_eq!(add_spec.simple_unit_name, Some("file_c".to_string()));
    }
}
