use std::{
    collections::BTreeMap,
    fmt::Display,
    io::{Result, Write},
    ops::Index,
};

use super::Visibility;
use crate::{
    analysis::{
        self,
        general::StatusedTypeId,
        imports::{ImportConditions, Imports},
        namespaces,
        special_functions::TraitInfo,
    },
    config::{derives::Derive, Config},
    env::Env,
    gir_version::VERSION,
    library::TypeId,
    nameutil::use_glib_type,
    version::Version,
    writer::primitives::tabs,
};

pub fn start_comments(w: &mut dyn Write, conf: &Config) -> Result<()> {
    if conf.single_version_file.is_some() {
        start_comments_no_version(w, conf)
    } else {
        single_version_file(w, conf, "// ")?;
        writeln!(w, "// DO NOT EDIT")
    }
}

pub fn start_comments_no_version(w: &mut dyn Write, conf: &Config) -> Result<()> {
    writeln!(
        w,
        "// This file was generated by gir (https://github.com/gtk-rs/gir)
{}// DO NOT EDIT",
        conf.girs_version
            .iter()
            .map(|info| {
                format!(
                    "// from {}{}\n",
                    info.gir_dir.display(),
                    info.get_repository_url()
                        .map_or_else(String::new, |url| format!(" ({})", url)),
                )
            })
            .collect::<String>()
    )
}

pub fn single_version_file(w: &mut dyn Write, conf: &Config, prefix: &str) -> Result<()> {
    write!(
        w,
        "{}Generated by gir (https://github.com/gtk-rs/gir @ {})
{}",
        prefix,
        VERSION,
        conf.girs_version
            .iter()
            .map(|info| {
                match (info.get_repository_url(), info.get_hash()) {
                    (Some(url), Some(hash)) => format!(
                        "{}from {} ({} @ {})\n",
                        prefix,
                        info.gir_dir.display(),
                        url,
                        hash,
                    ),
                    (None, Some(hash)) => {
                        format!("{}from {} (@ {})\n", prefix, info.gir_dir.display(), hash,)
                    }
                    _ => format!("{}from {}\n", prefix, info.gir_dir.display()),
                }
            })
            .collect::<String>(),
    )
}

pub fn uses(
    w: &mut dyn Write,
    env: &Env,
    imports: &Imports,
    outer_version: Option<Version>,
) -> Result<()> {
    writeln!(w)?;

    let mut grouped_imports: BTreeMap<(&str, Option<ImportConditions>), Vec<&str>> =
        BTreeMap::new();

    for (name, scope) in imports.iter() {
        let (crate_name, import) = name.split_once("::").unwrap();
        let mut scope = scope.clone();

        // The check here is needed to group unneeded version guards and allow grouping those imports
        scope.version = Version::if_stricter_than(scope.version, outer_version);
        let to_compare_with = env.config.min_required_version(env, None);
        scope.version = match (scope.version, to_compare_with) {
            (Some(v), Some(to_compare_v)) => {
                if v > to_compare_v {
                    scope.version
                } else {
                    None
                }
            }
            (Some(_), _) => scope.version,
            _ => None,
        };

        let key = if scope.constraints.is_empty() && scope.version.is_none() {
            (crate_name, None)
        } else {
            (crate_name, Some(scope))
        };
        grouped_imports
            .entry(key)
            .and_modify(|entry| entry.push(import))
            .or_insert_with(|| vec![import]);
    }

    for ((crate_name, scope), names) in grouped_imports.iter() {
        if !scope.is_none() {
            let scope = scope.as_ref().unwrap();
            if !scope.constraints.is_empty() {
                writeln!(
                    w,
                    "#[cfg(any({},feature = \"dox\"))]",
                    scope.constraints.join(", ")
                )?;
                writeln!(
                    w,
                    "#[cfg_attr(feature = \"dox\", doc(cfg({})))]",
                    scope.constraints.join(", ")
                )?;
            }
            version_condition(w, env, None, scope.version, false, 0)?;
        }
        writeln!(w, "use {crate_name}::{{{}}};", names.join(","))?;
    }

    Ok(())
}

fn format_parent_name(env: &Env, p: &StatusedTypeId) -> String {
    if p.type_id.ns_id == namespaces::MAIN {
        p.name.clone()
    } else {
        format!(
            "{krate}::{name}",
            krate = env.namespaces[p.type_id.ns_id].crate_name,
            name = p.name,
        )
    }
}

pub fn define_fundamental_type(
    w: &mut dyn Write,
    env: &Env,
    type_name: &str,
    glib_name: &str,
    glib_func_name: &str,
    ref_func: Option<&str>,
    unref_func: Option<&str>,
    parents: &[StatusedTypeId],
    visibility: Visibility,
) -> Result<()> {
    let sys_crate_name = env.main_sys_crate_name();
    writeln!(w, "{} {{", use_glib_type(env, "wrapper!"))?;
    doc_alias(w, glib_name, "", 1)?;
    writeln!(
        w,
        "\t{} struct {}(Shared<{}::{}>);",
        visibility, type_name, sys_crate_name, glib_name
    )?;
    writeln!(w)?;

    writeln!(w, "\tmatch fn {{")?;
    let (ref_fn, unref_fn, ptr, ffi_crate_name) = if parents.is_empty() {
        // it's the super-type, it must have a ref/unref functions
        (
            ref_func.unwrap().to_owned(),
            unref_func.unwrap().to_owned(),
            "ptr".to_owned(),
            sys_crate_name.to_owned(),
        )
    } else {
        let (ref_fn, unref_fn, ptr, ffi_crate_name) = parents
            .iter()
            .find_map(|p| {
                use crate::library::*;
                let type_ = env.library.type_(p.type_id);
                let parent_sys_crate_name = env.sys_crate_import(p.type_id);
                match type_ {
                    Type::Class(class) => Some((
                        class.ref_fn.as_ref().unwrap().clone(),
                        class.unref_fn.as_ref().unwrap().clone(),
                        format!(
                            "ptr as *mut {}::{}",
                            parent_sys_crate_name,
                            class.c_type.clone()
                        ),
                        parent_sys_crate_name,
                    )),
                    _ => None,
                }
            })
            .unwrap();
        // otherwise get that information from the parent
        (ref_fn, unref_fn, ptr, ffi_crate_name)
    };

    writeln!(
        w,
        "\t\tref => |ptr| {}::{}({}),",
        ffi_crate_name, ref_fn, ptr
    )?;
    writeln!(
        w,
        "\t\tunref => |ptr| {}::{}({}),",
        ffi_crate_name, unref_fn, ptr
    )?;

    writeln!(w, "\t}}")?;
    writeln!(w, "}}")?;

    // We can't use type_ from glib::wrapper! because that would auto-implement
    // Value traits which are often not the correct types
    writeln!(w, "\n")?;
    writeln!(
        w,
        "impl {} for {} {{",
        use_glib_type(env, "StaticType"),
        type_name
    )?;
    writeln!(w, "\tfn static_type() -> {} {{", use_glib_type(env, "Type"))?;
    writeln!(
        w,
        "\t\t unsafe {{ from_glib({}::{}()) }}",
        sys_crate_name, glib_func_name
    )?;
    writeln!(w, "\t}}")?;
    writeln!(w, "}}")?;
    Ok(())
}

pub fn define_object_type(
    w: &mut dyn Write,
    env: &Env,
    type_name: &str,
    glib_name: &str,
    glib_class_name: Option<&str>,
    glib_func_name: &str,
    is_interface: bool,
    parents: &[StatusedTypeId],
    visibility: Visibility,
) -> Result<()> {
    let sys_crate_name = env.main_sys_crate_name();
    let class_name = {
        if let Some(s) = glib_class_name {
            format!(", {}::{}", sys_crate_name, s)
        } else {
            String::new()
        }
    };

    let kind_name = if is_interface { "Interface" } else { "Object" };

    let parents: Vec<StatusedTypeId> = parents
        .iter()
        .filter(|p| !p.status.ignored())
        .cloned()
        .collect();

    writeln!(w, "{} {{", use_glib_type(env, "wrapper!"))?;
    doc_alias(w, glib_name, "", 1)?;
    if parents.is_empty() {
        writeln!(
            w,
            "\t{} struct {}({}<{}::{}{}>);",
            visibility, type_name, kind_name, sys_crate_name, glib_name, class_name
        )?;
    } else if is_interface {
        let prerequisites: Vec<String> =
            parents.iter().map(|p| format_parent_name(env, p)).collect();

        writeln!(
            w,
            "\t{} struct {}(Interface<{}::{}{}>) @requires {};",
            visibility,
            type_name,
            sys_crate_name,
            glib_name,
            class_name,
            prerequisites.join(", ")
        )?;
    } else {
        let interfaces: Vec<String> = parents
            .iter()
            .filter(|p| {
                use crate::library::*;

                matches!(
                    *env.library.type_(p.type_id),
                    Type::Interface { .. } if !p.status.ignored()
                )
            })
            .map(|p| format_parent_name(env, p))
            .collect();

        let parents: Vec<String> = parents
            .iter()
            .filter(|p| {
                use crate::library::*;

                matches!(
                    *env.library.type_(p.type_id),
                    Type::Class { .. } if !p.status.ignored()
                )
            })
            .map(|p| format_parent_name(env, p))
            .collect();

        let mut parents_string = String::new();
        if !parents.is_empty() {
            parents_string.push_str(format!(" @extends {}", parents.join(", ")).as_str());
        }

        if !interfaces.is_empty() {
            if !parents.is_empty() {
                parents_string.push(',');
            }
            parents_string.push_str(format!(" @implements {}", interfaces.join(", ")).as_str());
        }

        writeln!(
            w,
            "\t{} struct {}(Object<{}::{}{}>){};",
            visibility, type_name, sys_crate_name, glib_name, class_name, parents_string,
        )?;
    }
    writeln!(w)?;
    writeln!(w, "\tmatch fn {{")?;
    writeln!(
        w,
        "\t\ttype_ => || {}::{}(),",
        sys_crate_name, glib_func_name
    )?;
    writeln!(w, "\t}}")?;
    writeln!(w, "}}")?;

    Ok(())
}

fn define_boxed_type_internal(
    w: &mut dyn Write,
    env: &Env,
    type_name: &str,
    glib_name: &str,
    copy_fn: &TraitInfo,
    free_fn: &str,
    boxed_inline: bool,
    init_function_expression: &Option<String>,
    copy_into_function_expression: &Option<String>,
    clear_function_expression: &Option<String>,
    get_type_fn: Option<&str>,
    derive: &[Derive],
    visibility: Visibility,
) -> Result<()> {
    let sys_crate_name = env.main_sys_crate_name();
    writeln!(w, "{} {{", use_glib_type(env, "wrapper!"))?;

    derives(w, derive, 1)?;
    writeln!(
        w,
        "\t{} struct {}(Boxed{}<{}::{}>);",
        visibility,
        type_name,
        if boxed_inline { "Inline" } else { "" },
        sys_crate_name,
        glib_name
    )?;
    writeln!(w)?;
    writeln!(w, "\tmatch fn {{")?;
    let mut_ov = if copy_fn.first_parameter_mut {
        "mut_override(ptr)"
    } else {
        "ptr"
    };
    writeln!(
        w,
        "\t\tcopy => |ptr| {}::{}({}),",
        sys_crate_name, copy_fn.glib_name, mut_ov
    )?;
    writeln!(w, "\t\tfree => |ptr| {}::{}(ptr),", sys_crate_name, free_fn)?;

    if let (
        Some(init_function_expression),
        Some(copy_into_function_expression),
        Some(clear_function_expression),
    ) = (
        init_function_expression,
        copy_into_function_expression,
        clear_function_expression,
    ) {
        writeln!(w, "\t\tinit => {},", init_function_expression,)?;
        writeln!(w, "\t\tcopy_into => {},", copy_into_function_expression,)?;
        writeln!(w, "\t\tclear => {},", clear_function_expression,)?;
    }

    if let Some(get_type_fn) = get_type_fn {
        writeln!(w, "\t\ttype_ => || {}::{}(),", sys_crate_name, get_type_fn)?;
    }
    writeln!(w, "\t}}")?;
    writeln!(w, "}}")?;

    Ok(())
}

pub fn define_boxed_type(
    w: &mut dyn Write,
    env: &Env,
    type_name: &str,
    glib_name: &str,
    copy_fn: &TraitInfo,
    free_fn: &str,
    boxed_inline: bool,
    init_function_expression: &Option<String>,
    copy_into_function_expression: &Option<String>,
    clear_function_expression: &Option<String>,
    get_type_fn: Option<(String, Option<Version>)>,
    derive: &[Derive],
    visibility: Visibility,
) -> Result<()> {
    writeln!(w)?;

    if let Some((ref get_type_fn, get_type_version)) = get_type_fn {
        if get_type_version.is_some() {
            version_condition(w, env, None, get_type_version, false, 0)?;
            define_boxed_type_internal(
                w,
                env,
                type_name,
                glib_name,
                copy_fn,
                free_fn,
                boxed_inline,
                init_function_expression,
                copy_into_function_expression,
                clear_function_expression,
                Some(get_type_fn),
                derive,
                visibility,
            )?;

            writeln!(w)?;
            not_version_condition_no_dox(w, env, None, get_type_version, false, 0)?;
            define_boxed_type_internal(
                w,
                env,
                type_name,
                glib_name,
                copy_fn,
                free_fn,
                boxed_inline,
                init_function_expression,
                copy_into_function_expression,
                clear_function_expression,
                None,
                derive,
                visibility,
            )?;
        } else {
            define_boxed_type_internal(
                w,
                env,
                type_name,
                glib_name,
                copy_fn,
                free_fn,
                boxed_inline,
                init_function_expression,
                copy_into_function_expression,
                clear_function_expression,
                Some(get_type_fn),
                derive,
                visibility,
            )?;
        }
    } else {
        define_boxed_type_internal(
            w,
            env,
            type_name,
            glib_name,
            copy_fn,
            free_fn,
            boxed_inline,
            init_function_expression,
            copy_into_function_expression,
            clear_function_expression,
            None,
            derive,
            visibility,
        )?;
    }

    Ok(())
}

pub fn define_auto_boxed_type(
    w: &mut dyn Write,
    env: &Env,
    type_name: &str,
    glib_name: &str,
    boxed_inline: bool,
    init_function_expression: &Option<String>,
    copy_into_function_expression: &Option<String>,
    clear_function_expression: &Option<String>,
    get_type_fn: &str,
    derive: &[Derive],
    visibility: Visibility,
) -> Result<()> {
    let sys_crate_name = env.main_sys_crate_name();
    writeln!(w)?;
    writeln!(w, "{} {{", use_glib_type(env, "wrapper!"))?;
    derives(w, derive, 1)?;
    writeln!(
        w,
        "\t{} struct {}(Boxed{}<{}::{}>);",
        visibility,
        type_name,
        if boxed_inline { "Inline" } else { "" },
        sys_crate_name,
        glib_name
    )?;
    writeln!(w)?;
    writeln!(w, "\tmatch fn {{")?;
    writeln!(
        w,
        "\t\tcopy => |ptr| {}({}::{}(), ptr as *mut _) as *mut {}::{},",
        use_glib_type(env, "gobject_ffi::g_boxed_copy"),
        sys_crate_name,
        get_type_fn,
        sys_crate_name,
        glib_name
    )?;
    writeln!(
        w,
        "\t\tfree => |ptr| {}({}::{}(), ptr as *mut _),",
        use_glib_type(env, "gobject_ffi::g_boxed_free"),
        sys_crate_name,
        get_type_fn
    )?;

    if let (
        Some(init_function_expression),
        Some(copy_into_function_expression),
        Some(clear_function_expression),
    ) = (
        init_function_expression,
        copy_into_function_expression,
        clear_function_expression,
    ) {
        writeln!(w, "\t\tinit => {},", init_function_expression,)?;
        writeln!(w, "\t\tcopy_into => {},", copy_into_function_expression,)?;
        writeln!(w, "\t\tclear => {},", clear_function_expression,)?;
    }

    writeln!(w, "\t\ttype_ => || {}::{}(),", sys_crate_name, get_type_fn)?;
    writeln!(w, "\t}}")?;
    writeln!(w, "}}")?;

    Ok(())
}

fn define_shared_type_internal(
    w: &mut dyn Write,
    env: &Env,
    type_name: &str,
    glib_name: &str,
    ref_fn: &str,
    unref_fn: &str,
    get_type_fn: Option<&str>,
    derive: &[Derive],
    visibility: Visibility,
) -> Result<()> {
    let sys_crate_name = env.main_sys_crate_name();
    writeln!(w, "{} {{", use_glib_type(env, "wrapper!"))?;
    derives(w, derive, 1)?;
    writeln!(
        w,
        "\t{} struct {}(Shared<{}::{}>);",
        visibility, type_name, sys_crate_name, glib_name
    )?;
    writeln!(w)?;
    writeln!(w, "\tmatch fn {{")?;
    writeln!(w, "\t\tref => |ptr| {}::{}(ptr),", sys_crate_name, ref_fn)?;
    writeln!(
        w,
        "\t\tunref => |ptr| {}::{}(ptr),",
        sys_crate_name, unref_fn
    )?;
    if let Some(get_type_fn) = get_type_fn {
        writeln!(w, "\t\ttype_ => || {}::{}(),", sys_crate_name, get_type_fn)?;
    }
    writeln!(w, "\t}}")?;
    writeln!(w, "}}")?;

    Ok(())
}

pub fn define_shared_type(
    w: &mut dyn Write,
    env: &Env,
    type_name: &str,
    glib_name: &str,
    ref_fn: &str,
    unref_fn: &str,
    get_type_fn: Option<(String, Option<Version>)>,
    derive: &[Derive],
    visibility: Visibility,
) -> Result<()> {
    writeln!(w)?;

    if let Some((ref get_type_fn, get_type_version)) = get_type_fn {
        if get_type_version.is_some() {
            version_condition(w, env, None, get_type_version, false, 0)?;
            define_shared_type_internal(
                w,
                env,
                type_name,
                glib_name,
                ref_fn,
                unref_fn,
                Some(get_type_fn),
                derive,
                visibility,
            )?;

            writeln!(w)?;
            not_version_condition_no_dox(w, env, None, get_type_version, false, 0)?;
            define_shared_type_internal(
                w, env, type_name, glib_name, ref_fn, unref_fn, None, derive, visibility,
            )?;
        } else {
            define_shared_type_internal(
                w,
                env,
                type_name,
                glib_name,
                ref_fn,
                unref_fn,
                Some(get_type_fn),
                derive,
                visibility,
            )?;
        }
    } else {
        define_shared_type_internal(
            w, env, type_name, glib_name, ref_fn, unref_fn, None, derive, visibility,
        )?;
    }

    Ok(())
}

pub fn cfg_deprecated(
    w: &mut dyn Write,
    env: &Env,
    type_tid: Option<TypeId>,
    deprecated: Option<Version>,
    commented: bool,
    indent: usize,
) -> Result<()> {
    if let Some(s) = cfg_deprecated_string(env, type_tid, deprecated, commented, indent) {
        writeln!(w, "{}", s)?;
    }
    Ok(())
}

pub fn cfg_deprecated_string(
    env: &Env,
    type_tid: Option<TypeId>,
    deprecated: Option<Version>,
    commented: bool,
    indent: usize,
) -> Option<String> {
    let comment = if commented { "//" } else { "" };
    deprecated.map(|v| {
        if env.is_too_low_version(type_tid.map(|t| t.ns_id), Some(v)) {
            format!("{}{}#[deprecated = \"Since {}\"]", tabs(indent), comment, v)
        } else {
            format!(
                "{}{}#[cfg_attr({}, deprecated = \"Since {}\")]",
                tabs(indent),
                comment,
                v.to_cfg(None),
                v,
            )
        }
    })
}

pub fn version_condition(
    w: &mut dyn Write,
    env: &Env,
    ns_id: Option<u16>,
    version: Option<Version>,
    commented: bool,
    indent: usize,
) -> Result<()> {
    if let Some(s) = version_condition_string(env, ns_id, version, commented, indent) {
        writeln!(w, "{}", s)?;
    }
    Ok(())
}

pub fn version_condition_no_doc(
    w: &mut dyn Write,
    env: &Env,
    ns_id: Option<u16>,
    version: Option<Version>,
    commented: bool,
    indent: usize,
) -> Result<()> {
    let to_compare_with = env.config.min_required_version(env, ns_id);
    let should_generate = match (version, to_compare_with) {
        (Some(v), Some(to_compare_v)) => v > to_compare_v,
        (Some(_), _) => true,
        _ => false,
    };
    if should_generate {
        // Prefix with the crate name if it's not the main one
        let namespace_name = ns_id.and_then(|ns| {
            if ns == namespaces::MAIN {
                None
            } else {
                Some(env.namespaces.index(ns).crate_name.clone())
            }
        });
        if let Some(s) = cfg_condition_string_no_doc(
            Some(&version.unwrap().to_cfg(namespace_name.as_deref())),
            commented,
            indent,
        ) {
            writeln!(w, "{}", s)?;
        }
    }
    Ok(())
}
pub fn version_condition_doc(
    w: &mut dyn Write,
    env: &Env,
    version: Option<Version>,
    commented: bool,
    indent: usize,
) -> Result<()> {
    match version {
        Some(v) if v > env.config.min_cfg_version => {
            if let Some(s) = cfg_condition_string_doc(Some(&v.to_cfg(None)), commented, indent) {
                writeln!(w, "{}", s)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub fn version_condition_string(
    env: &Env,
    ns_id: Option<u16>,
    version: Option<Version>,
    commented: bool,
    indent: usize,
) -> Option<String> {
    let to_compare_with = env.config.min_required_version(env, ns_id);
    let should_generate = match (version, to_compare_with) {
        (Some(v), Some(to_compare_v)) => v > to_compare_v,
        (Some(_), _) => true,
        _ => false,
    };
    if should_generate {
        // Prefix with the crate name if it's not the main one
        let namespace_name = ns_id.and_then(|ns| {
            if ns == namespaces::MAIN {
                None
            } else {
                Some(env.namespaces.index(ns).crate_name.clone())
            }
        });
        cfg_condition_string(
            Some(&version.unwrap().to_cfg(namespace_name.as_deref())),
            commented,
            indent,
        )
    } else {
        None
    }
}

pub fn not_version_condition(
    w: &mut dyn Write,
    version: Option<Version>,
    commented: bool,
    indent: usize,
) -> Result<()> {
    if let Some(s) = version.and_then(|v| {
        cfg_condition_string(Some(&format!("not({})", v.to_cfg(None))), commented, indent)
    }) {
        writeln!(w, "{}", s)?;
    }
    Ok(())
}

pub fn not_version_condition_no_dox(
    w: &mut dyn Write,
    env: &Env,
    ns_id: Option<u16>,
    version: Option<Version>,
    commented: bool,
    indent: usize,
) -> Result<()> {
    if let Some(v) = version {
        let comment = if commented { "//" } else { "" };
        let namespace_name = ns_id.and_then(|ns| {
            if ns == namespaces::MAIN {
                None
            } else {
                Some(env.namespaces.index(ns).crate_name.clone())
            }
        });
        let s = format!(
            "{}{}#[cfg(not(any({}, feature = \"dox\")))]",
            tabs(indent),
            comment,
            v.to_cfg(namespace_name.as_deref())
        );
        writeln!(w, "{}", s)?;
    }
    Ok(())
}

pub fn cfg_condition(
    w: &mut dyn Write,
    cfg_condition: Option<&(impl Display + ?Sized)>,
    commented: bool,
    indent: usize,
) -> Result<()> {
    if let Some(s) = cfg_condition_string(cfg_condition, commented, indent) {
        writeln!(w, "{}", s)?;
    }
    Ok(())
}

pub fn cfg_condition_no_doc(
    w: &mut dyn Write,
    cfg_condition: Option<&(impl Display + ?Sized)>,
    commented: bool,
    indent: usize,
) -> Result<()> {
    if let Some(s) = cfg_condition_string_no_doc(cfg_condition, commented, indent) {
        writeln!(w, "{}", s)?;
    }
    Ok(())
}

pub fn cfg_condition_string_no_doc(
    cfg_condition: Option<&(impl Display + ?Sized)>,
    commented: bool,
    indent: usize,
) -> Option<String> {
    cfg_condition.map(|cfg| {
        let comment = if commented { "//" } else { "" };
        format!(
            "{0}{1}#[cfg(any({2}, feature = \"dox\"))]",
            tabs(indent),
            comment,
            cfg,
        )
    })
}

pub fn cfg_condition_doc(
    w: &mut dyn Write,
    cfg_condition: Option<&(impl Display + ?Sized)>,
    commented: bool,
    indent: usize,
) -> Result<()> {
    if let Some(s) = cfg_condition_string_doc(cfg_condition, commented, indent) {
        writeln!(w, "{}", s)?;
    }
    Ok(())
}

pub fn cfg_condition_string_doc(
    cfg_condition: Option<&(impl Display + ?Sized)>,
    commented: bool,
    indent: usize,
) -> Option<String> {
    cfg_condition.map(|cfg| {
        let comment = if commented { "//" } else { "" };
        format!(
            "{0}{1}#[cfg_attr(feature = \"dox\", doc(cfg({2})))]",
            tabs(indent),
            comment,
            cfg,
        )
    })
}

pub fn cfg_condition_string(
    cfg_condition: Option<&(impl Display + ?Sized)>,
    commented: bool,
    indent: usize,
) -> Option<String> {
    cfg_condition.map(|_| {
        format!(
            "{}\n{}",
            cfg_condition_string_no_doc(cfg_condition, commented, indent).unwrap(),
            cfg_condition_string_doc(cfg_condition, commented, indent).unwrap(),
        )
    })
}

pub fn derives(w: &mut dyn Write, derives: &[Derive], indent: usize) -> Result<()> {
    for derive in derives {
        let s = match &derive.cfg_condition {
            Some(condition) => format!(
                "#[cfg_attr({}, derive({}))]",
                condition,
                derive.names.join(", ")
            ),
            None => format!("#[derive({})]", derive.names.join(", ")),
        };
        writeln!(w, "{}{}", tabs(indent), s)?;
    }
    Ok(())
}

pub fn doc_alias(w: &mut dyn Write, name: &str, comment_prefix: &str, indent: usize) -> Result<()> {
    writeln!(
        w,
        "{}{}#[doc(alias = \"{}\")]",
        tabs(indent),
        comment_prefix,
        name,
    )
}

pub fn doc_hidden(
    w: &mut dyn Write,
    doc_hidden: bool,
    comment_prefix: &str,
    indent: usize,
) -> Result<()> {
    if doc_hidden {
        writeln!(w, "{}{}#[doc(hidden)]", tabs(indent), comment_prefix)
    } else {
        Ok(())
    }
}

pub fn allow_deprecated(
    w: &mut dyn Write,
    allow_deprecated: Option<Version>,
    commented: bool,
    indent: usize,
) -> Result<()> {
    if allow_deprecated.is_some() {
        writeln!(
            w,
            "{}{}#[allow(deprecated)]",
            tabs(indent),
            if commented { "//" } else { "" }
        )
    } else {
        Ok(())
    }
}

pub fn write_vec<T: Display>(w: &mut dyn Write, v: &[T]) -> Result<()> {
    for s in v {
        writeln!(w, "{}", s)?;
    }
    Ok(())
}

pub fn declare_default_from_new(
    w: &mut dyn Write,
    env: &Env,
    name: &str,
    functions: &[analysis::functions::Info],
    has_builder: bool,
) -> Result<()> {
    if let Some(func) = functions.iter().find(|f| {
        !f.hidden
            && f.status.need_generate()
            && f.name == "new"
            // Cannot generate Default implementation for Option<>
            && f.ret.parameter.as_ref().map_or(false, |x| !*x.lib_par.nullable)
    }) {
        if func.parameters.rust_parameters.is_empty() {
            writeln!(w)?;
            version_condition(w, env, None, func.version, false, 0)?;
            writeln!(
                w,
                "impl Default for {} {{
                     fn default() -> Self {{
                         Self::new()
                     }}
                 }}",
                name
            )?;
        } else if has_builder {
            // create an alternative default implementation the uses `glib::object::Object::new()`
            writeln!(w)?;
            version_condition(w, env, None, func.version, false, 0)?;
            writeln!(
                w,
                "impl Default for {0} {{
                     fn default() -> Self {{
                         glib::object::Object::new::<Self>(&[])
                     }}
                 }}",
                name
            )?;
        }
    }

    Ok(())
}

/// Escapes string in format suitable for placing inside double quotes.
pub fn escape_string(s: &str) -> String {
    let mut es = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        match c {
            '\"' | '\\' => es.push('\\'),
            _ => (),
        }
        es.push(c);
    }
    es
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_string() {
        assert_eq!(escape_string(""), "");
        assert_eq!(escape_string("no escaping here"), "no escaping here");
        assert_eq!(escape_string(r#"'"\"#), r#"'\"\\"#);
    }
}
