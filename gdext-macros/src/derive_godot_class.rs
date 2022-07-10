use crate::util::{bail, ident, KvMap, KvValue};
use crate::{util, ParseResult};
use proc_macro2::{Ident, Punct, Span, TokenStream};
use quote::quote;
use quote::spanned::Spanned;
use venial::{Attribute, NamedField, Struct, StructFields, TyExpr};

pub fn transform(input: TokenStream) -> ParseResult<TokenStream> {
    let decl = venial::parse_declaration(input)?;

    let class = decl
        .as_struct()
        .ok_or(venial::Error::new("Not a valid struct"))?;

    let struct_cfg = parse_struct_attributes(class)?;
    let fields = parse_fields(class)?;

    let base_ty = &struct_cfg.base_ty;
    let class_name = &class.name;
    let class_name_str = class.name.to_string();
    let default = match struct_cfg.new_mode {
        GodotDefaultMode::AutoGenerated => create_default_auto(class_name, fields),
        GodotDefaultMode::FnNew => create_default_fn(class_name),
        GodotDefaultMode::None => TokenStream::new(),
    };

    Ok(quote! {
        impl gdext_class::traits::GodotClass for #class_name {
            type Base = gdext_class::api::#base_ty;
            type Declarer = gdext_class::dom::UserDomain;
            type Mem = <Self::Base as gdext_class::traits::GodotClass>::Mem;

            fn class_name() -> String {
                #class_name_str.to_string()
            }
        }
        #default
        // impl GodotExtensionClass for #class_name {
        //     fn virtual_call(_name: &str) -> sys::GDNativeExtensionClassCallVirtual {
        //         todo!()
        //     }
        //     fn register_methods() {}
        // }

    })
}

/// Returns the name of the base and the default mode
fn parse_struct_attributes(class: &Struct) -> ParseResult<ClassAttributes> {
    let mut base = ident("RefCounted");
    let mut new_mode = GodotDefaultMode::AutoGenerated;

    // #[godot] attribute on struct
    if let Some((span, mut map)) = parse_godot_attr(&class.attributes)? {
        if let Some(kv_value) = map.remove("base") {
            if let KvValue::Ident(override_base) = kv_value {
                base = override_base;
            } else {
                bail("Invalid value for 'base' argument", span)?;
            }
        }

        if let Some(kv_value) = map.remove("new") {
            match kv_value {
                KvValue::Ident(ident) if ident == "fn" => new_mode = GodotDefaultMode::FnNew,
                KvValue::Ident(ident) if ident == "none" => new_mode = GodotDefaultMode::None,
                _ => bail(
                    "Invalid value for 'new' argument; must be 'fn' or 'none'",
                    span,
                )?,
            }
        }
        /*
        else if let Some(kv_value) = map.remove("new") {
            if let KvValue::None = kv_value {
                has_default = false;
            } else {
                bail("'no_default' argument must not have any value", span)?;
            }
        }
         */
    }

    Ok(ClassAttributes {
        base_ty: base,
        new_mode,
    })
}

/// Returns field names and 1 base field, if available
fn parse_fields(class: &Struct) -> ParseResult<Fields> {
    let mut all_field_names = vec![];
    let mut exported_fields = vec![];
    let mut base_field = Option::<ExportedField>::None;

    let fields: Vec<(NamedField, Punct)> = match &class.fields {
        StructFields::Unit => {
            vec![]
        }
        StructFields::Tuple(_) => bail(
            "#[derive(GodotClass)] not supported for tuple structs",
            &class.fields,
        )?,
        StructFields::Named(fields) => fields.fields.inner.clone(),
    };

    // Attributes on struct fields
    for (field, _punct) in fields {
        let mut is_base = false;

        // #[base] or #[export]
        for attr in field.attributes.iter() {
            if let Some(path) = attr.get_single_path_segment() {
                if path.to_string() == "base" {
                    is_base = true;
                    if let Some(prev_base) = base_field {
                        bail(
                            &format!(
                                "#[base] allowed for at most 1 field, already applied to '{}'",
                                prev_base.name
                            ),
                            attr,
                        )?;
                    }
                    base_field = Some(ExportedField::new(&field))
                } else if path.to_string() == "export" {
                    exported_fields.push(ExportedField::new(&field))
                }
            }
        }

        // Exported or Rust-only fields
        if !is_base {
            all_field_names.push(field.name.clone())
        }
    }

    Ok(Fields {
        all_field_names,
        base_field,
    })
}

/// Parses a `#[godot(...)]` attribute
fn parse_godot_attr(attributes: &Vec<Attribute>) -> ParseResult<Option<(Span, KvMap)>> {
    let mut godot_attr = None;
    for attr in attributes.iter() {
        let path = &attr.path;
        if path.len() == 1 || path[0].to_string() == "godot" {
            if godot_attr.is_some() {
                bail(
                    "Only one #[godot] attribute per item (struct, fn, ...) allowed",
                    attr,
                )?;
            }

            let map = util::parse_kv_group(&attr.value)?;
            godot_attr = Some((attr.__span(), map));
        }
    }
    Ok(godot_attr)
}

// ----------------------------------------------------------------------------------------------------------------------------------------------
// General helpers

struct ClassAttributes {
    base_ty: Ident,
    new_mode: GodotDefaultMode,
}

struct Fields {
    all_field_names: Vec<Ident>,
    base_field: Option<ExportedField>,
}

enum GodotDefaultMode {
    AutoGenerated,
    FnNew,
    None,
}

struct ExportedField {
    name: Ident,
    _ty: TyExpr,
}

impl ExportedField {
    fn new(field: &NamedField) -> Self {
        Self {
            name: field.name.clone(),
            _ty: field.ty.clone(),
        }
    }
}

fn create_default_auto(class_name: &Ident, fields: Fields) -> TokenStream {
    let base_init = if let Some(ExportedField { name, .. }) = fields.base_field {
        quote! { #name: base, }
    } else {
        TokenStream::new()
    };

    let rest_init = fields.all_field_names.into_iter().map(|field| {
        quote! { #field: std::default::Default::default(), }
    });

    quote! {
        impl gdext_class::traits::GodotDefault for #class_name {
            fn godot_default(base: gdext_class::Base<Self::Base>) -> Self {
                Self {
                    #( #rest_init )*
                    #base_init
                }
            }
        }
    }
}

fn create_default_fn(class_name: &Ident) -> TokenStream {
    quote! {
        impl gdext_class::traits::GodotDefault for #class_name {
            fn godot_default(base: gdext_class::Base<Self::Base>) -> Self {
                <Self as gdext_class::traits::GodotMethods>::init(base)
            }
        }
    }
}
