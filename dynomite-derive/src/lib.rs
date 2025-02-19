//! Provides procedural macros for deriving dynomite types for your structs and enum types
//!
//! # Examples
//!
//! ```ignore
//! use dynomite::{Item, FromAttributes, Attributes};
//! use dynomite::dynamodb::AttributeValue;
//!
//! // derive Item
//! #[derive(Item, PartialEq, Debug, Clone)]
//! struct Person {
//!   #[dynomite(partition_key)] id: String
//! }
//!
//!   let person = Person { id: "123".into() };
//!   // convert person to string keys and attribute values
//!   let attributes: Attributes = person.clone().into();
//!   // convert attributes into person type
//!   assert_eq!(person, Person::from_attrs(attributes).unwrap());
//!
//!   // dynamodb types require only primary key attributes and may contain
//!   // other fields. when looking up items only those key attributes are required
//!   // dynomite derives a new {Name}Key struct for your which contains
//!   // only those and also implements Item
//!   let key = PersonKey { id: "123".into() };
//!   let key_attributes: Attributes = key.clone().into();
//!   // convert attributes into person type
//!   assert_eq!(key, PersonKey::from_attrs(key_attributes).unwrap());
//! ```

extern crate proc_macro;

mod attr;
use attr::Attr;

use proc_macro::TokenStream;
use proc_macro2::Span;
use proc_macro_error::ResultExt;
use quote::{quote, ToTokens};
use syn::{
    punctuated::Punctuated,
    Attribute,
    Data::{Enum, Struct},
    DataStruct, DeriveInput, Field, Fields, Ident, Token, Variant, Visibility,
};

/// A Field and all its extracted dynomite derive attrs
#[derive(Clone)]
struct ItemField<'a> {
    field: &'a Field,
    attrs: Vec<Attr>,
}

impl<'a> ItemField<'a> {
    fn new(field: &'a Field) -> Self {
        let attrs = parse_attrs(&field.attrs);
        Self { field, attrs }
    }

    fn is_partition_key(&self) -> bool {
        self.attrs
            .iter()
            .any(|attr| matches!(attr, Attr::PartitionKey(_)))
    }

    fn is_sort_key(&self) -> bool {
        self.attrs
            .iter()
            .any(|attr| matches!(attr, Attr::SortKey(_)))
    }

    fn is_default_when_absent(&self) -> bool {
        self.attrs
            .iter()
            .any(|attr| matches!(attr, Attr::Default(_)))
    }

    fn deser_name(&self) -> String {
        let ItemField { field, attrs } = self;
        attrs
            .iter()
            .find_map(|attr| match attr {
                Attr::Rename(_, lit) => Some(lit.value()),
                _ => None,
            })
            .unwrap_or_else(|| {
                field
                    .ident
                    .as_ref()
                    .expect("should have an identifier")
                    .to_string()
            })
    }
}

fn parse_attrs(all_attrs: &[Attribute]) -> Vec<Attr> {
    all_attrs
        .iter()
        .filter(|attr| attr.path.is_ident("dynomite"))
        .flat_map(|attr| {
            attr.parse_args_with(Punctuated::<Attr, Token![,]>::parse_terminated)
                .unwrap_or_abort()
        })
        .collect()
}

/// Derives `dynomite::Item` type for struts with named fields
///
/// # Attributes
///
/// * `#[dynomite(partition_key)]` - required attribute, expected to be applied the target [partition attribute](https://docs.aws.amazon.com/amazondynamodb/latest/developerguide/HowItWorks.CoreComponents.html#HowItWorks.CoreComponents.PrimaryKey) field with an derivable DynamoDB attribute value of String, Number or Binary
/// * `#[dynomite(sort_key)]` - optional attribute, may be applied to one target [sort attribute](https://docs.aws.amazon.com/amazondynamodb/latest/developerguide/HowItWorks.CoreComponents.html#HowItWorks.CoreComponents.SecondaryIndexes) field with an derivable DynamoDB attribute value of String, Number or Binary
/// * `#[dynomite(rename = "actualName")]` - optional attribute, may be applied any item attribute field, useful when the DynamoDB table you're interfacing with has attributes whose names don't following Rust's naming conventions
///
/// # Panics
///
/// This proc macro will panic when applied to other types
#[proc_macro_error::proc_macro_error]
#[proc_macro_derive(Item, attributes(partition_key, sort_key, dynomite))]
pub fn derive_item(input: TokenStream) -> TokenStream {
    let ast = syn::parse_macro_input!(input);

    let gen = match expand_item(ast) {
        Ok(g) => g,
        Err(e) => return e.to_compile_error().into(),
    };

    gen.into_token_stream().into()
}

/// similar in spirit to `#[derive(Item)]` except these are exempt from declaring
/// partition and sort keys
#[proc_macro_error::proc_macro_error]
#[proc_macro_derive(Attributes, attributes(dynomite))]
pub fn derive_attributes(input: TokenStream) -> TokenStream {
    let ast = syn::parse_macro_input!(input);

    let gen = match expand_attributes(ast) {
        Ok(g) => g,
        Err(e) => return e.to_compile_error().into(),
    };

    gen.into_token_stream().into()
}

/// Derives `dynomite::Attribute` for enum types
///
/// # Panics
///
/// This proc macro will panic when applied to other types
#[proc_macro_error::proc_macro_error]
#[proc_macro_derive(Attribute)]
pub fn derive_attribute(input: TokenStream) -> TokenStream {
    let ast = syn::parse_macro_input!(input);
    let gen = expand_attribute(ast);
    gen.into_token_stream().into()
}

fn expand_attribute(ast: DeriveInput) -> impl ToTokens {
    let name = &ast.ident;
    match ast.data {
        Enum(variants) => {
            make_dynomite_attr(name, &variants.variants.into_iter().collect::<Vec<_>>())
        }
        _ => panic!("Dynomite Attributes can only be generated for enum types"),
    }
}

/// ```rust,ignore
/// impl ::dynomite::Attribute for Name {
///   fn into_attr(self) -> ::dynomite::dynamodb::AttributeValue {
///     let arm = match self {
///        Name::Variant => "Variant".to_string()
///     };
///     ::dynomite::dynamodb::AttributeValue {
///        s: Some(arm),
///        ..Default::default()
///     }
///   }
///   fn from_attr(value: ::dynomite::dynamodb::AttributeValue) -> Result<Self, ::dynomite::AttributeError> {
///     value.s.ok_or(::dynomite::AttributeError::InvalidType)
///       .and_then(|value| match &value[..] {
///          "Variant" => Ok(Name::Variant),
///          _ => Err(::dynomite::AttributeError::InvalidFormat)
///       })
///   }
/// }
/// ```
fn make_dynomite_attr(
    name: &Ident,
    variants: &[Variant],
) -> impl ToTokens {
    let attr = quote!(::dynomite::Attribute);
    let err = quote!(::dynomite::AttributeError);
    let into_match_arms = variants.iter().map(|var| {
        let vname = &var.ident;
        quote! {
            #name::#vname => stringify!(#vname).to_string(),
        }
    });
    let from_match_arms = variants.iter().map(|var| {
        let vname = &var.ident;
        quote! {
            stringify!(#vname) => ::std::result::Result::Ok(#name::#vname),
        }
    });

    quote! {
        impl #attr for #name {
            fn into_attr(self) -> ::dynomite::dynamodb::AttributeValue {
                let arm = match self {
                    #(#into_match_arms)*
                };
                ::dynomite::dynamodb::AttributeValue {
                    s: ::std::option::Option::Some(arm),
                    ..::std::default::Default::default()
                }
            }
            fn from_attr(value: ::dynomite::dynamodb::AttributeValue) -> ::std::result::Result<Self, #err> {
                value.s.ok_or(::dynomite::AttributeError::InvalidType)
                    .and_then(|value| match &value[..] {
                        #(#from_match_arms)*
                        _ => ::std::result::Result::Err(::dynomite::AttributeError::InvalidFormat)
                    })
            }
        }
    }
}

fn expand_attributes(ast: DeriveInput) -> syn::Result<impl ToTokens> {
    use syn::spanned::Spanned as _;
    let name = &ast.ident;
    match ast.data {
        Struct(DataStruct { fields, .. }) => match fields {
            Fields::Named(named) => {
                make_dynomite_attributes(name, &named.named.into_iter().collect::<Vec<_>>())
            }
            fields => Err(syn::Error::new(
                fields.span(),
                "Dynomite Attributes require named fields",
            )),
        },
        _ => panic!("Dynomite Attributes can only be generated for structs"),
    }
}

fn expand_item(ast: DeriveInput) -> syn::Result<impl ToTokens> {
    use syn::spanned::Spanned as _;
    let name = &ast.ident;
    let vis = &ast.vis;
    match ast.data {
        Struct(DataStruct { fields, .. }) => match fields {
            Fields::Named(named) => {
                make_dynomite_item(vis, name, &named.named.into_iter().collect::<Vec<_>>())
            }
            fields => Err(syn::Error::new(
                fields.span(),
                "Dynomite Items require named fields",
            )),
        },
        _ => panic!("Dynomite Items can only be generated for structs"),
    }
}

fn make_dynomite_attributes(
    name: &Ident,
    fields: &[Field],
) -> syn::Result<impl ToTokens> {
    let item_fields = fields.iter().map(ItemField::new).collect::<Vec<_>>();
    // impl ::dynomite::FromAttributes for Name
    let from_attribute_map = get_from_attributes_trait(name, &item_fields)?;
    // impl From<Name> for ::dynomite::Attributes
    let to_attribute_map = get_to_attribute_map_trait(name, &item_fields)?;
    // impl Attribute for Name (these are essentially just a map)
    let attribute = quote!(::dynomite::Attribute);
    let impl_attribute = quote! {
        impl #attribute for #name {
            fn into_attr(self: Self) -> ::dynomite::AttributeValue {
                ::dynomite::AttributeValue {
                    m: Some(self.into()),
                    ..::dynomite::AttributeValue::default()
                }
            }
            fn from_attr(value: ::dynomite::AttributeValue) -> Result<Self, ::dynomite::AttributeError> {
                use ::dynomite::FromAttributes;
                value
                    .m
                    .ok_or(::dynomite::AttributeError::InvalidType)
                    .and_then(Self::from_attrs)
            }
        }
    };

    Ok(quote! {
        #from_attribute_map
        #to_attribute_map
        #impl_attribute
    })
}

fn make_dynomite_item(
    vis: &Visibility,
    name: &Ident,
    fields: &[Field],
) -> syn::Result<impl ToTokens> {
    let item_fields = fields.iter().map(ItemField::new).collect::<Vec<_>>();
    // all items must have 1 primary_key
    let partition_key_count = item_fields.iter().filter(|f| f.is_partition_key()).count();
    if partition_key_count != 1 {
        return Err(syn::Error::new(
            name.span(),
            format!(
                "All Item's must declare one and only one partition_key. The `{}` Item declared {}",
                name, partition_key_count
            ),
        ));
    }
    // impl Item for Name + NameKey struct
    let dynamodb_traits = get_dynomite_item_traits(vis, name, &item_fields)?;
    // impl ::dynomite::FromAttributes for Name
    let from_attribute_map = get_from_attributes_trait(name, &item_fields)?;
    // impl From<Name> for ::dynomite::Attributes
    let to_attribute_map = get_to_attribute_map_trait(name, &item_fields)?;

    Ok(quote! {
        #from_attribute_map
        #to_attribute_map
        #dynamodb_traits
    })
}

// impl From<Name> for ::dynomite::Attributes {
//    fn from(n: Name) ->  Self {
//      ...
//    }
// }
//
fn get_to_attribute_map_trait(
    name: &Ident,
    fields: &[ItemField],
) -> syn::Result<impl ToTokens> {
    let attributes = quote!(::dynomite::Attributes);
    let from = quote!(::std::convert::From);
    let to_attribute_map = get_to_attribute_map_function(name, fields)?;

    Ok(quote! {
        impl #from<#name> for #attributes {
            #to_attribute_map
        }
    })
}

// generates the `from(...)` method for attribute map From conversion
//
// fn from(item: Foo) -> Self {
//   let mut values = Self::new();
//   values.insert(
//     "foo".to_string(),
//     ::dynomite::Attribute::into_attr(item.field)
//   );
//   ...
//   values
// }
fn get_to_attribute_map_function(
    name: &Ident,
    fields: &[ItemField],
) -> syn::Result<impl ToTokens> {
    let to_attribute_value = quote!(::dynomite::Attribute::into_attr);

    let field_conversions = fields
        .iter()
        .map(|field| {
            let field_deser_name = field.deser_name();

            let field_ident = &field.field.ident;
            Ok(quote! {
                values.insert(
                    #field_deser_name.to_string(),
                    #to_attribute_value(item.#field_ident)
                );
            })
        })
        .collect::<syn::Result<Vec<_>>>()?;

    Ok(quote! {
        fn from(item: #name) -> Self {
            let mut values = Self::new();
            #(#field_conversions)*
            values
        }
    })
}

/// ```rust,ignore
/// impl ::dynomite::FromAttributes for Name {
///   fn from_attrs(mut item: ::dynomite::Attributes) -> Result<Self, ::dynomite::Error> {
///     Ok(Self {
///        field_name: ::dynomite::Attribute::from_attr(
///           item.remove("field_deser_name").ok_or(Error::MissingField { name: "field_deser_name".into() })?
///        )
///      })
///   }
/// }
/// ```
fn get_from_attributes_trait(
    name: &Ident,
    fields: &[ItemField],
) -> syn::Result<impl ToTokens> {
    let from_attrs = quote!(::dynomite::FromAttributes);
    let from_attribute_map = get_from_attributes_function(fields)?;

    Ok(quote! {
        impl #from_attrs for #name {
            #from_attribute_map
        }
    })
}

fn get_from_attributes_function(fields: &[ItemField]) -> syn::Result<impl ToTokens> {
    let attributes = quote!(::dynomite::Attributes);
    let from_attribute_value = quote!(::dynomite::Attribute::from_attr);
    let err = quote!(::dynomite::AttributeError);

    let field_conversions = fields.iter().map(|field| {
        // field has #[dynomite(renameField = "...")] attribute
        let field_deser_name = field.deser_name();

        let field_ident = &field.field.ident;
        if field.is_default_when_absent() {
            Ok(quote! {
                #field_ident: match attrs.remove(#field_deser_name) {
                    Some(field) => #from_attribute_value(field)?,
                    _ => ::std::default::Default::default()
                }
            })
        } else {
            Ok(quote! {
                #field_ident: #from_attribute_value(
                    attrs.remove(#field_deser_name)
                        .ok_or(::dynomite::AttributeError::MissingField { name: #field_deser_name.to_string() })?
                )?
            })
        }
    }).collect::<syn::Result<Vec<_>>>()?;

    Ok(quote! {
        fn from_attrs(mut attrs: #attributes) -> ::std::result::Result<Self, #err> {
            ::std::result::Result::Ok(Self {
                #(#field_conversions),*
            })
        }
    })
}

fn get_dynomite_item_traits(
    vis: &Visibility,
    name: &Ident,
    fields: &[ItemField],
) -> syn::Result<impl ToTokens> {
    let impls = get_item_impls(vis, name, fields)?;

    Ok(quote! {
        #impls
    })
}

fn get_item_impls(
    vis: &Visibility,
    name: &Ident,
    fields: &[ItemField],
) -> syn::Result<impl ToTokens> {
    // impl ::dynomite::Item for Name ...
    let item_trait = get_item_trait(name, fields)?;
    // pub struct NameKey ...
    let key_struct = get_key_struct(vis, name, fields)?;

    Ok(quote! {
        #item_trait
        #key_struct
    })
}

/// ```rust,ignore
/// impl ::dynomite::Item for Name {
///   fn key(&self) -> ::std::collections::HashMap<String, ::dynomite::dynamodb::AttributeValue> {
///     let mut keys = ::std::collections::HashMap::new();
///     keys.insert("field_deser_name", to_attribute_value(field));
///     keys
///   }
/// }
/// ```
fn get_item_trait(
    name: &Ident,
    fields: &[ItemField],
) -> syn::Result<impl ToTokens> {
    let item = quote!(::dynomite::Item);
    let attribute_map = quote!(
        ::std::collections::HashMap<String, ::dynomite::dynamodb::AttributeValue>
    );
    let partition_key_field = fields.iter().find(|f| f.is_partition_key());
    let sort_key_field = fields.iter().find(|f| f.is_sort_key());
    let partition_key_insert = partition_key_field.map(get_key_inserter).transpose()?;
    let sort_key_insert = sort_key_field.map(get_key_inserter).transpose()?;

    Ok(partition_key_field
        .map(|_| {
            quote! {
                impl #item for #name {
                    fn key(&self) -> #attribute_map {
                        let mut keys = ::std::collections::HashMap::new();
                        #partition_key_insert
                        #sort_key_insert
                        keys
                    }
                }
            }
        })
        .unwrap_or_else(proc_macro2::TokenStream::new))
}

/// ```rust,ignore
/// keys.insert(
///   "field_deser_name", to_attribute_value(field)
/// );
/// ```
fn get_key_inserter(field: &ItemField) -> syn::Result<impl ToTokens> {
    let to_attribute_value = quote!(::dynomite::Attribute::into_attr);

    let field_deser_name = field.deser_name();
    let field_ident = &field.field.ident;
    Ok(quote! {
        keys.insert(
            #field_deser_name.to_string(),
            #to_attribute_value(self.#field_ident.clone())
        );
    })
}

/// ```rust,ignore
/// #[derive(Item, Debug, Clone, PartialEq)]
/// pub struct NameKey {
///    partition_key_field,
///    range_key
/// }
/// ```
fn get_key_struct(
    vis: &Visibility,
    name: &Ident,
    fields: &[ItemField],
) -> syn::Result<impl ToTokens> {
    let name = Ident::new(&format!("{}Key", name), Span::call_site());

    let partition_key_field = fields
        .iter()
        .find(|field| field.is_partition_key())
        .cloned()
        .map(|field| {
            // clone because this is a new struct
            // note: this in inherits field attrs so that
            // we retain dynomite(rename = "xxx")
            let field = field.field.clone();
            quote! {
                #field
            }
        });

    let sort_key_field = fields
        .iter()
        .find(|field| field.is_sort_key())
        .cloned()
        .map(|field| {
            // clone because this is a new struct
            // note: this in inherits field attrs so that
            // we retain dynomite(rename = "xxx")
            let field = field.field.clone();
            quote! {
                #field
            }
        });

    Ok(partition_key_field
        .map(|partition_key_field| {
            quote! {
                #[derive(::dynomite::Attributes, Debug, Clone, PartialEq)]
                #vis struct #name {
                    #partition_key_field,
                    #sort_key_field
                }
            }
        })
        .unwrap_or_else(proc_macro2::TokenStream::new))
}
