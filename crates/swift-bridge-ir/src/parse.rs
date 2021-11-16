use crate::errors::{ParseError, ParseErrors};
use crate::extern_rust::{ExternRustSection, ExternRustSectionType};
use crate::{ExternFn, SelfDesc, SwiftBridgeModule, TypeMethod};
use proc_macro2::Ident;
use quote::ToTokens;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::ops::Deref;
use syn::parse::{Parse, ParseStream};
use syn::{FnArg, ForeignItem, ForeignItemFn, Item, ItemMod, Pat, PatType, Receiver, Token, Type};

impl Parse for SwiftBridgeModule {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let module_and_errors: SwiftBridgeModuleAndErrors = input.parse()?;

        module_and_errors.errors.combine_all()?;

        Ok(module_and_errors.module)
    }
}

pub(crate) struct SwiftBridgeModuleAndErrors {
    pub module: SwiftBridgeModule,
    pub errors: ParseErrors,
}

enum AbiLang {
    Rust,
    Swift,
}

// A method with self, &self, or &mut self that we came across.
// We'll need to infer the type based on the `type Foo` declaration within the extern Rust block.
struct MethodToInfer {
    func: ForeignItemFn,
    receiver: Receiver,
    args: Vec<PatType>,
}

impl Parse for SwiftBridgeModuleAndErrors {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut errors = ParseErrors::new();

        if let Ok(item_mod) = input.parse::<ItemMod>() {
            let module_name = item_mod.ident;

            let mut extern_rust = vec![];

            for outer_mod_item in item_mod.content.unwrap().1 {
                match outer_mod_item {
                    Item::ForeignMod(mut foreign_mod) => {
                        if foreign_mod.abi.name.is_none() {
                            errors.push(ParseError::AbiNameMissing {
                                extern_token: foreign_mod.abi.extern_token,
                            });
                            continue;
                        }

                        let mut rust_types: HashMap<String, ExternRustSectionType> = HashMap::new();
                        let mut free_functions = vec![];

                        let abi_name = foreign_mod.abi.name.unwrap();

                        let abi_name = match abi_name.value().as_str() {
                            "Rust" => AbiLang::Rust,
                            "Swift" => AbiLang::Swift,
                            _ => {
                                errors.push(ParseError::AbiNameInvalid { abi_name });
                                continue;
                            }
                        };

                        foreign_mod.items.sort_by(|a, b| {
                            if matches!(a, ForeignItem::Type(_)) {
                                Ordering::Less
                            } else {
                                Ordering::Greater
                            }
                        });

                        for foreign_mod_item in foreign_mod.items {
                            match foreign_mod_item {
                                ForeignItem::Fn(func) => {
                                    let first_input = func.sig.inputs.iter().next();

                                    if let Some(first) = first_input {
                                        match first {
                                            FnArg::Receiver(recv) => {
                                                let method_self: SelfDesc = recv.clone().into();

                                                if rust_types.len() == 1 {
                                                    let ty =
                                                        rust_types.iter_mut().next().unwrap().1;
                                                    ty.methods.push(TypeMethod {
                                                        this: Some(method_self),
                                                        func: ExternFn { func },
                                                    });
                                                } else {
                                                    errors.push(ParseError::AmbiguousSelf {
                                                        self_: recv.clone(),
                                                    });
                                                }
                                            }
                                            FnArg::Typed(arg) => {
                                                match arg.pat.deref() {
                                                    Pat::Ident(pat_ident) => {
                                                        if pat_ident.ident.to_string() == "self" {
                                                            let self_ty = arg.ty.deref();

                                                            match self_ty {
                                                                Type::Path(path) => {
                                                                    let self_ty_string = path
                                                                        .path
                                                                        .segments
                                                                        .to_token_stream()
                                                                        .to_string();

                                                                    if let Some(ty) = rust_types
                                                                        .get_mut(&self_ty_string)
                                                                    {
                                                                        ty.methods
                                                                            .push(TypeMethod {
                                                                            this: Some(SelfDesc {
                                                                                reference: None,
                                                                                mutability: None,
                                                                            }),
                                                                            func: ExternFn { func },
                                                                        });
                                                                    } else {
                                                                        todo!("Handle type not present in this extern")
                                                                    }
                                                                }
                                                                Type::Reference(type_ref) => {
                                                                    let self_ty =
                                                                        type_ref.elem.deref();

                                                                    match self_ty {
                                                                        Type::Path(path) => {
                                                                            let self_ty_string = path
                                                                               .path
                                                                               .segments
                                                                               .to_token_stream()
                                                                               .to_string();

                                                                            if let Some(ty) =
                                                                                rust_types.get_mut(
                                                                                    &self_ty_string,
                                                                                )
                                                                            {
                                                                                ty.methods
                                                                                   .push(TypeMethod {
                                                                                       this: Some(
                                                                                           SelfDesc {
                                                                                               reference: Some(type_ref.and_token),
                                                                                               mutability: type_ref.mutability
                                                                                           },
                                                                                       ),
                                                                                       func: ExternFn { func },
                                                                                   });
                                                                            } else {
                                                                                todo!("Handle type not present in this extern")
                                                                            }
                                                                        }
                                                                        _ => {
                                                                            todo!("Add a test that hits this block..")
                                                                        }
                                                                    };
                                                                }
                                                                _ => {}
                                                            };
                                                        } else {
                                                            free_functions.push(ExternFn { func });
                                                        }
                                                    }
                                                    _ => {}
                                                };
                                            }
                                        }
                                    } else {
                                        let mut associated_to = None;

                                        for attr in func.attrs.iter() {
                                            let attr: SwiftBridgeAttr = attr.parse_args()?;

                                            match attr {
                                                SwiftBridgeAttr::AssociatedTo(ty) => {
                                                    associated_to = Some(ty);
                                                }
                                            }
                                        }

                                        if let Some(associated_to) = associated_to {
                                            rust_types
                                                .get_mut(&associated_to.to_string())
                                                .unwrap()
                                                .methods
                                                .push(TypeMethod {
                                                    this: None,
                                                    func: ExternFn { func },
                                                })
                                        } else {
                                            free_functions.push(ExternFn { func });
                                        }
                                    }
                                }
                                ForeignItem::Type(foreign_ty) => {
                                    let ty_name = foreign_ty.ident.to_string();

                                    rust_types
                                        .insert(ty_name, ExternRustSectionType::new(foreign_ty));
                                }
                                _ => {}
                            }
                        }

                        let section = ExternRustSection {
                            types: rust_types.into_values().collect(),
                            free_functions,
                        };
                        extern_rust.push(section);
                    }
                    _ => {
                        //
                        todo!("Return an error that the module may only contain `extern` blocks.")
                    }
                };
            }

            let module = SwiftBridgeModule {
                name: module_name,
                extern_rust,
            };
            Ok(SwiftBridgeModuleAndErrors { module, errors })
        } else {
            return Err(syn::Error::new_spanned(
                input.to_string(),
                "Only modules and impl blocks are supported.",
            ));
        }
    }
}

enum SwiftBridgeAttr {
    AssociatedTo(Ident),
}

impl Parse for SwiftBridgeAttr {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let key: Ident = input.parse()?;

        // TODO: Better error handling..
        assert_eq!(key.to_string(), "associated_to");

        input.parse::<Token![=]>()?;
        let value: Ident = input.parse()?;

        Ok(SwiftBridgeAttr::AssociatedTo(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::ParseError;
    use proc_macro2::TokenStream;
    use quote::quote;
    use syn::parse_quote;

    /// Verify that we can parse a SwiftBridgeModule from an empty module.
    #[test]
    fn parse_empty_module() {
        let tokens = quote! {
            mod foo { }
        };
        let module: SwiftBridgeModule = parse_quote!(#tokens);

        assert_eq!(module.name.to_string(), "foo");
    }

    /// Verify that we store an error if no abi name was provided.
    #[test]
    fn error_if_no_abi_name_provided_for_an_extern_block() {
        let tokens = quote! {
            mod foo {
                extern {}
            }
        };
        let errors = parse_errors(tokens);

        assert_eq!(errors.len(), 1);
        match errors[0] {
            ParseError::AbiNameMissing { .. } => {}
            _ => panic!(),
        }
    }

    /// Verify that we store an error if the abi name isn't Rust or Swift.
    #[test]
    fn error_if_invalid_abi_name() {
        let tokens = quote! {
            mod foo {
                extern "SomeAbi" {}
            }
        };
        let errors = parse_errors(tokens);

        assert_eq!(errors.len(), 1);
        match &errors[0] {
            ParseError::AbiNameInvalid { abi_name } => {
                assert_eq!(abi_name.value(), "SomeAbi");
            }
            _ => panic!(),
        }
    }

    /// Verify that we can parse a Rust type declaration.
    #[test]
    fn rust_type_declaration() {
        let tokens = quote! {
            mod foo {
                extern "Rust" {
                    type Foo;
                }
            }
        };

        let module = parse_ok(tokens);

        assert_eq!(module.extern_rust[0].types[0].ty.ident.to_string(), "Foo");
    }

    /// Verify that we can parse a Rust type's methods.
    /// We test all of the possible ways we can specify self.
    #[test]
    fn parses_rust_self_methods() {
        let tests = vec![
            quote! { fn bar (self); },
            quote! { fn bar (&self); },
            quote! { fn bar (&mut self); },
            quote! { fn bar (self: Foo); },
            quote! { fn bar (self: &Foo); },
            quote! { fn bar (self: &mut Foo); },
            quote! {
                #[swift_bridge(associated_to = Foo)]
                fn bar ();
            },
        ];

        for fn_definition in tests {
            let tokens = quote! {
                mod foo {
                    extern "Rust" {
                        type Foo;

                        #fn_definition
                    }
                }
            };

            let module = parse_ok(tokens);

            let ty = &module.extern_rust[0].types[0];
            assert_eq!(ty.ty.ident.to_string(), "Foo");

            assert_eq!(
                ty.methods.len(),
                1,
                "Failed not parse {} into an associated method.",
                quote! {#fn_definition}.to_string()
            );
        }
    }

    /// Verify that we can parse a freestanding Rust function declaration.
    #[test]
    fn parse_rust_freestanding_function_no_args() {
        let tokens = quote! {
            mod foo {
                extern "Rust" {
                    fn bar () -> u8;
                }
            }
        };

        let module = parse_ok(tokens);

        assert_eq!(module.extern_rust[0].free_functions.len(), 1);
    }

    /// Verify that we can parse a freestanding Rust function declaration that has one arg.
    #[test]
    fn parse_rust_freestanding_function_one_arg() {
        let tokens = quote! {
            mod foo {
                extern "Rust" {
                    fn bar (bazz: u32) -> u8;
                }
            }
        };

        let module = parse_ok(tokens);

        assert_eq!(module.extern_rust[0].free_functions.len(), 1);
    }

    /// Verify that if an extern Rust block has more than one type, we push errors for any methods
    /// that have an ambiguous self.
    #[test]
    fn error_if_method_has_ambiguous_self() {
        let tokens = quote! {
            mod foo {
                extern "Rust" {
                    type SomeType;
                    type AnotherType;

                    fn a (self);
                    fn b (&self);
                    fn c (&mut self);
                }
            }
        };

        let errors = parse_errors(tokens);

        assert_eq!(errors.len(), 3);

        for idx in 0..3 {
            match &errors[idx] {
                ParseError::AmbiguousSelf { self_: _ } => {}
                _ => panic!(),
            };
        }
    }

    /// Verify that annotated self methods get parsed.
    #[test]
    fn disambiguate_method() {
        let tokens = quote! {
            mod foo {
                extern "Rust" {
                    type SomeType;
                    type AnotherType;

                    fn a (self: SomeType);
                    fn b (self: &SomeType);
                    fn c (self: &mut AnotherType);
                }
            }
        };

        let mut module = parse_ok(tokens);

        let types = &mut module.extern_rust[0].types;
        types.sort_by(|a, b| {
            a.ty.ident
                .to_string()
                .cmp(&b.ty.ident.to_string())
                .reverse()
        });

        assert_eq!(types[0].methods.len(), 2);
        assert_eq!(types[1].methods.len(), 1);
    }

    #[test]
    fn todo() {
        todo!(
            r#"
Lots more tests.. Add tests for all of the todo!()'s in the parse method..
First refactor out the parser into smaller pieces.. then more exhaustively test these pieces.
        "#
        )
    }

    fn parse_ok(tokens: TokenStream) -> SwiftBridgeModule {
        let parsed: SwiftBridgeModuleAndErrors = parse_quote!(#tokens);
        parsed.module
    }

    fn parse_errors(tokens: TokenStream) -> ParseErrors {
        let parsed: SwiftBridgeModuleAndErrors = parse_quote!(#tokens);
        parsed.errors
    }
}